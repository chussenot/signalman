//! `GET /readyz`: can this replica do its job right now? A liveness probe
//! (`/healthz`) says the process runs; readiness says the upstreams it needs
//! answer with the credentials it holds. Each check is the cheapest
//! authenticated call the upstream offers: incident.io `GET /v1/identity`,
//! TypeSafe `GET /v1/models`, and, when a catalog is configured, one
//! Backstage `by-query` for a single component. Every check has its own
//! deadline, they run concurrently, and the whole report is cached so probe
//! storms and many replicas do not turn readiness into load on the
//! upstreams. A failed check stays cached too, for the same reason. The
//! checks go through the same clients as a triage, retry policy included,
//! so the deadline covers the retries: a deadline shorter than the client's
//! backoff reports `timed out` where a longer one would name the transport
//! error the last attempt saw.
//!
//! The trade-off is deliberate and documented in `docs/operations.md`: an
//! upstream outage marks every replica unready, so the Service stops
//! accepting deliveries it could not process anyway; incident.io retries
//! non-2xx for 24 hours, so nothing is lost, and a bad API key shows up as
//! "not ready" at rollout instead of at the first alert.

use std::time::{Duration, Instant};

use jiff::Timestamp;
use serde::Serialize;
use tokio::sync::Mutex;

use crate::backstage;
use crate::incidentio::Triager;

/// How long a report is reused when nothing else is configured.
pub const DEFAULT_CACHE: Duration = Duration::from_secs(30);

/// Per-upstream deadline when nothing else is configured.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(3);

/// What [`Probe::new`] takes (`server.readiness_*` in the configuration).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Settings {
    /// How long the last report is reused; zero checks on every request.
    pub cache: Duration,
    /// Deadline for one upstream check.
    pub timeout: Duration,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            cache: DEFAULT_CACHE,
            timeout: DEFAULT_TIMEOUT,
        }
    }
}

/// One upstream's answer.
#[derive(Debug, Clone, Serialize)]
pub struct Upstream {
    /// `typesafe`, `incidentio` or `backstage`.
    pub service: &'static str,
    /// Whether it answered successfully within the deadline.
    pub ok: bool,
    /// How long the check took, whichever way it ended.
    pub latency_ms: u64,
    /// Why it failed: the client's error, or the deadline.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// The body of `GET /readyz`.
#[derive(Debug, Clone, Serialize)]
pub struct Report {
    /// Every upstream answered.
    pub ready: bool,
    /// When the checks ran (RFC 3339).
    pub checked_at: String,
    /// Whether this is a reused report rather than a fresh check.
    pub cached: bool,
    /// One entry per configured upstream.
    pub upstreams: Vec<Upstream>,
}

/// The readiness probe: the clients to check and the cached last report.
pub struct Probe {
    typesafe: crate::Client,
    incidentio: crate::incidentio::Client,
    backstage: Option<backstage::Client>,
    settings: Settings,
    cache: Mutex<Option<(Instant, Report)>>,
}

impl std::fmt::Debug for Probe {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Probe")
            .field("settings", &self.settings)
            .field("backstage", &self.backstage.is_some())
            .finish_non_exhaustive()
    }
}

impl Probe {
    /// Check the same upstreams `triager` uses, with the same credentials.
    pub fn new(triager: &Triager, settings: Settings) -> Self {
        Self {
            typesafe: triager.typesafe.clone(),
            incidentio: triager.incidentio.clone(),
            backstage: triager.backstage.as_ref().map(|e| e.client.clone()),
            settings,
            cache: Mutex::new(None),
        }
    }

    /// The current report: the cached one while it is fresh, otherwise a new
    /// check. Concurrent callers wait for the one check in flight rather
    /// than each running their own.
    pub async fn report(&self) -> Report {
        let mut cache = self.cache.lock().await;
        if let Some((at, report)) = cache.as_ref()
            && at.elapsed() < self.settings.cache
        {
            let mut reused = report.clone();
            reused.cached = true;
            return reused;
        }
        let report = self.check().await;
        *cache = Some((Instant::now(), report.clone()));
        report
    }

    #[tracing::instrument(name = "readiness.check", skip_all)]
    async fn check(&self) -> Report {
        let timeout = self.settings.timeout;
        let typesafe = probe("typesafe", timeout, async {
            self.typesafe
                .list_models()
                .await
                .map(|_| ())
                .map_err(|e| e.to_string())
        });
        let incidentio = probe("incidentio", timeout, async {
            self.incidentio
                .identity()
                .await
                .map(|_| ())
                .map_err(|e| e.to_string())
        });
        let backstage = async {
            let client = self.backstage.as_ref()?;
            Some(
                probe("backstage", timeout, async {
                    client
                        .query(&[&[("kind", Some("component"))]], Some("kind"), 1)
                        .await
                        .map(|_| ())
                        .map_err(|e| e.to_string())
                })
                .await,
            )
        };
        let (typesafe, incidentio, backstage) = tokio::join!(typesafe, incidentio, backstage);
        let upstreams: Vec<Upstream> = [Some(typesafe), Some(incidentio), backstage]
            .into_iter()
            .flatten()
            .collect();
        let ready = upstreams.iter().all(|u| u.ok);
        for failed in upstreams.iter().filter(|u| !u.ok) {
            tracing::warn!(
                service = failed.service,
                error = failed.error.as_deref().unwrap_or(""),
                latency_ms = failed.latency_ms,
                "readiness check failed"
            );
        }
        Report {
            ready,
            checked_at: Timestamp::now().to_string(),
            cached: false,
            upstreams,
        }
    }
}

/// Run one check under its deadline and shape the answer.
async fn probe(
    service: &'static str,
    timeout: Duration,
    call: impl Future<Output = Result<(), String>>,
) -> Upstream {
    let started = Instant::now();
    let outcome = match tokio::time::timeout(timeout, call).await {
        Ok(result) => result,
        Err(_) => Err(format!("timed out after {:.1} s", timeout.as_secs_f64())),
    };
    Upstream {
        service,
        ok: outcome.is_ok(),
        latency_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        error: outcome.err(),
    }
}
