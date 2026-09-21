//! The change feed: what was deployed, reconfigured or toggled recently,
//! pushed to signalman by whatever tool did it.
//!
//! `caused_by_change` is only asked when `alert.recent_changes` is
//! non-empty, and nothing filled it in the webhook flow until now. Rather
//! than teach signalman every continuous-delivery tool's API, it accepts
//! change events on `POST /changes` from any of them (Argo CD notifications,
//! Flux, a GitHub Actions step, a Backstage scaffolder action, a shell
//! script) and keeps a bounded, in-memory window. At triage time the
//! changes that touched the alerting component, or the whole platform, in
//! the last `window` become `alert.recent_changes` (decision 0007).
//!
//! In memory only, per replica: a change survives a restart no better than
//! the `webhook-id` set does, and the consequence is the same as before
//! this module existed, a `caused_by_change` question that is not asked.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use jiff::{SignedDuration, Timestamp};
use serde::{Deserialize, Serialize};

/// Environment variable holding the bearer token `POST /changes` requires.
/// Unset disables the feed. A secret: environment only.
pub const TOKEN_ENV: &str = "SIGNALMAN_CHANGES_TOKEN";

/// Most changes kept, oldest evicted first.
pub const DEFAULT_CAPACITY: usize = 1_000;

/// How far back a change may lie to be offered as a cause. Deploys cause
/// incidents hours later more often than minutes later.
pub const DEFAULT_WINDOW: Duration = Duration::from_secs(120 * 60);

/// Most changes put in the state per alert. Each is a line of input tokens.
pub const DEFAULT_MAX: usize = 10;

/// One change, as posted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Change {
    /// When it happened (RFC 3339). Defaults to receipt time when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<String>,
    /// `deploy`, `config`, `flag`, `infra`, `migration`, or anything short.
    pub kind: String,
    /// The component or service it touched, as the alert labels name it.
    /// `None` means platform-wide: offered to every alert.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub component: Option<String>,
    /// One line: what changed, version, actor.
    pub summary: String,
    /// The tool that posted it (`argocd`, `github-actions`, `flux`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Link to the deploy, pull request or commit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

impl Change {
    /// The line the model reads. Time first so ordering is visible, then
    /// kind and summary, then the component and a link when known.
    pub fn to_state_line(&self) -> String {
        let mut s = String::new();
        if let Some(at) = &self.at {
            s.push_str(at);
            s.push(' ');
        }
        s.push_str(&self.kind);
        s.push(' ');
        s.push_str(self.summary.trim());
        if let Some(c) = &self.component {
            s.push_str(" (");
            s.push_str(c);
            s.push(')');
        }
        if let Some(u) = &self.url {
            s.push(' ');
            s.push_str(u);
        }
        s
    }

    fn instant(&self) -> Option<Timestamp> {
        self.at.as_deref()?.parse().ok()
    }
}

/// A `Change` with a normalised timestamp. Stored newest last.
#[derive(Debug, Clone)]
struct Stored {
    at: Timestamp,
    change: Change,
}

/// The bounded, shared window of changes. Cheap to clone; one per process.
#[derive(Debug, Clone)]
pub struct ChangeLog {
    inner: Arc<Mutex<VecDeque<Stored>>>,
    capacity: usize,
}

impl Default for ChangeLog {
    fn default() -> Self {
        Self::new(DEFAULT_CAPACITY)
    }
}

impl ChangeLog {
    /// Empty log keeping at most `capacity` changes.
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(VecDeque::new())),
            capacity: capacity.max(1),
        }
    }

    /// Record a change. A missing or unparsable `at` becomes `now`, in
    /// RFC 3339 to the second, so the state line stays readable. Returns
    /// the stored change.
    pub fn record(&self, mut change: Change, now: Timestamp) -> Change {
        let at = change.instant().unwrap_or(now);
        change.at = Some(at.strftime("%Y-%m-%dT%H:%M:%SZ").to_string());
        let mut log = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Keep chronological order so `recent` can stop early.
        let pos = log.partition_point(|s| s.at <= at);
        log.insert(
            pos,
            Stored {
                at,
                change: change.clone(),
            },
        );
        while log.len() > self.capacity {
            log.pop_front();
        }
        change
    }

    /// Changes within `window` before `now` that touched one of `hints`
    /// (case-insensitive) or no component at all, newest first, at most
    /// `max`. An empty `hints` still returns the platform-wide changes.
    pub fn recent(
        &self,
        hints: &[String],
        window: Duration,
        now: Timestamp,
        max: usize,
    ) -> Vec<Change> {
        if window.is_zero() || max == 0 {
            return Vec::new();
        }
        let Ok(window) = SignedDuration::try_from(window) else {
            return Vec::new();
        };
        let Ok(since) = now.checked_sub(window) else {
            return Vec::new();
        };
        let log = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        log.iter()
            .rev()
            .take_while(|s| s.at >= since)
            .filter(|s| s.at <= now)
            .filter(|s| match &s.change.component {
                None => true,
                Some(c) => hints.iter().any(|h| h.eq_ignore_ascii_case(c)),
            })
            .take(max)
            .map(|s| s.change.clone())
            .collect()
    }

    /// Everything currently held, oldest first.
    pub fn all(&self) -> Vec<Change> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .map(|s| s.change.clone())
            .collect()
    }

    /// Number of changes held.
    pub fn len(&self) -> usize {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }

    /// True when nothing is held.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// The bearer token the feed requires.
#[derive(Clone)]
pub struct FeedToken(String);

impl std::fmt::Debug for FeedToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("FeedToken(<redacted>)")
    }
}

impl FeedToken {
    /// From an explicit value; empty is `None`.
    pub fn new(token: impl Into<String>) -> Option<Self> {
        let t: String = token.into();
        let t = t.trim().to_owned();
        (!t.is_empty()).then_some(Self(t))
    }

    /// From `SIGNALMAN_CHANGES_TOKEN`; `None` when unset or empty.
    pub fn from_env() -> Option<Self> {
        std::env::var(TOKEN_ENV).ok().and_then(Self::new)
    }

    /// Constant-time comparison against an `Authorization` header value.
    pub fn accepts(&self, authorization: Option<&str>) -> bool {
        use subtle::ConstantTimeEq;
        let Some(h) = authorization else {
            return false;
        };
        let Some(presented) = h.strip_prefix("Bearer ").map(str::trim) else {
            return false;
        };
        presented.len() == self.0.len() && presented.as_bytes().ct_eq(self.0.as_bytes()).into()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn ts(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    fn change(at: Option<&str>, component: Option<&str>, summary: &str) -> Change {
        Change {
            at: at.map(str::to_owned),
            kind: "deploy".into(),
            component: component.map(str::to_owned),
            summary: summary.into(),
            source: Some("test".into()),
            url: None,
        }
    }

    #[test]
    fn recent_filters_by_window_component_and_cap_newest_first() {
        let log = ChangeLog::new(10);
        let now = ts("2026-09-21T12:00:00Z");
        log.record(
            change(Some("2026-09-21T11:50:00Z"), Some("checkout-api"), "v2"),
            now,
        );
        log.record(
            change(Some("2026-09-21T09:00:00Z"), Some("checkout-api"), "v1"),
            now,
        ); // outside 2h
        log.record(
            change(Some("2026-09-21T11:30:00Z"), None, "autoscaler 1.31"),
            now,
        );
        log.record(
            change(Some("2026-09-21T11:55:00Z"), Some("payments"), "v9"),
            now,
        );
        log.record(
            change(Some("2026-09-21T13:00:00Z"), Some("checkout-api"), "future"),
            now,
        );

        let got = log.recent(&["Checkout-API".into()], DEFAULT_WINDOW, now, 10);
        let lines: Vec<String> = got.iter().map(Change::to_state_line).collect();
        assert_eq!(
            lines,
            vec![
                "2026-09-21T11:50:00Z deploy v2 (checkout-api)",
                "2026-09-21T11:30:00Z deploy autoscaler 1.31",
            ]
        );
        // No hints: platform-wide only.
        assert_eq!(log.recent(&[], DEFAULT_WINDOW, now, 10).len(), 1);
        // Cap.
        assert_eq!(
            log.recent(&["checkout-api".into()], DEFAULT_WINDOW, now, 1)
                .len(),
            1
        );
        // Zero window disables.
        assert!(
            log.recent(&["checkout-api".into()], Duration::ZERO, now, 10)
                .is_empty()
        );
    }

    #[test]
    fn record_defaults_the_time_and_evicts_the_oldest() {
        let log = ChangeLog::new(2);
        let now = ts("2026-09-21T12:00:00Z");
        let stored = log.record(change(None, None, "no time"), now);
        assert_eq!(stored.at.as_deref(), Some("2026-09-21T12:00:00Z"));
        let stored = log.record(change(Some("garbage"), None, "bad time"), now);
        assert_eq!(stored.at.as_deref(), Some("2026-09-21T12:00:00Z"));
        log.record(change(Some("2026-09-21T11:00:00Z"), None, "older"), now);
        // Capacity 2: the oldest (11:00) is evicted after insertion in order.
        assert_eq!(log.len(), 2);
        assert!(log.all().iter().all(|c| c.summary != "older"));
    }

    #[test]
    fn feed_token_checks_bearer_in_constant_time_shape() {
        let t = FeedToken::new("s3cret").unwrap();
        assert!(t.accepts(Some("Bearer s3cret")));
        assert!(!t.accepts(Some("Bearer s3cre")));
        assert!(!t.accepts(Some("Basic s3cret")));
        assert!(!t.accepts(None));
        assert!(FeedToken::new("  ").is_none());
        assert_eq!(format!("{t:?}"), "FeedToken(<redacted>)");
    }

    #[test]
    fn state_line_is_time_kind_summary_component_url() {
        let c = Change {
            at: Some("2026-09-21T11:42:00Z".into()),
            kind: "deploy".into(),
            component: Some("checkout-api".into()),
            summary: " checkout-api v2.31.0 by alice ".into(),
            source: Some("argocd".into()),
            url: Some("https://argocd.example.com/applications/checkout-api".into()),
        };
        assert_eq!(
            c.to_state_line(),
            "2026-09-21T11:42:00Z deploy checkout-api v2.31.0 by alice (checkout-api) https://argocd.example.com/applications/checkout-api"
        );
    }
}
