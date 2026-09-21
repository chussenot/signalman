//! GitLab adapter: project webhooks in their documented shapes become
//! changes. `X-Gitlab-Event` names the kind; the body follows
//! <https://docs.gitlab.com/user/project/integrations/webhook_events/>.
//!
//! Mapped: deployments that succeeded (`deploy`), feature flag toggles
//! (`flag`), releases (`release`) and merge requests that were merged
//! (`merge`). Everything else, including deployments still running and
//! merge requests merely updated, is not a change that happened and is
//! reported as ignored so GitLab sees a 2xx and does not retry.
//!
//! GitLab timestamps come in three shapes (`2021-04-28 21:50:00 +0200`,
//! `2020-11-02 12:55:12 UTC`, RFC 3339); all are normalised to RFC 3339.

use jiff::Timestamp;
use jiff::fmt::strtime;
use serde::Deserialize;

use super::Change;

/// Header naming the event kind.
pub const EVENT_HEADER: &str = "x-gitlab-event";
/// Header carrying the secret token configured on the webhook.
pub const TOKEN_HEADER: &str = "x-gitlab-token";

#[derive(Debug, Default, Deserialize)]
struct Project {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    path_with_namespace: Option<String>,
    #[serde(default)]
    web_url: Option<String>,
}

impl Project {
    /// Lowercased project name: the component as most alert labels spell it.
    fn component(&self) -> Option<String> {
        self.name
            .as_deref()
            .or_else(|| self.path_with_namespace.as_deref()?.rsplit('/').next())
            .map(|n| n.trim().to_ascii_lowercase())
            .filter(|n| !n.is_empty())
    }
}

#[derive(Debug, Default, Deserialize)]
struct User {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    username: Option<String>,
}

impl User {
    fn label(&self) -> Option<&str> {
        self.username.as_deref().or(self.name.as_deref())
    }
}

#[derive(Debug, Deserialize)]
struct Deployment {
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    status_changed_at: Option<String>,
    #[serde(default)]
    environment: Option<String>,
    #[serde(default)]
    short_sha: Option<String>,
    #[serde(default)]
    commit_title: Option<String>,
    #[serde(default)]
    deployable_url: Option<String>,
    #[serde(default)]
    project: Project,
    #[serde(default)]
    user: User,
}

#[derive(Debug, Deserialize)]
struct FeatureFlag {
    #[serde(default)]
    object_attributes: FlagAttributes,
    #[serde(default)]
    project: Project,
    #[serde(default)]
    user: User,
}

#[derive(Debug, Default, Deserialize)]
struct FlagAttributes {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    active: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct Release {
    #[serde(default)]
    action: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    tag: Option<String>,
    #[serde(default)]
    released_at: Option<String>,
    #[serde(default)]
    created_at: Option<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    project: Project,
}

#[derive(Debug, Deserialize)]
struct MergeRequest {
    #[serde(default)]
    object_attributes: MergeAttributes,
    #[serde(default)]
    project: Project,
    #[serde(default)]
    user: User,
}

#[derive(Debug, Default, Deserialize)]
struct MergeAttributes {
    #[serde(default)]
    action: Option<String>,
    #[serde(default)]
    iid: Option<u64>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    target_branch: Option<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    updated_at: Option<String>,
    #[serde(default)]
    merged_at: Option<String>,
    #[serde(default)]
    merge_commit_sha: Option<String>,
}

/// Why a delivery was not turned into a change.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The body does not match the event's documented shape.
    #[error("invalid {event} payload: {source}")]
    Shape {
        /// The `X-Gitlab-Event` value.
        event: String,
        /// Cause.
        #[source]
        source: serde_json::Error,
    },
    /// The project has no usable name to match a component on.
    #[error("{0} payload has no project name")]
    NoProject(String),
}

/// The outcome of one delivery.
#[derive(Debug, PartialEq, Eq)]
pub enum Delivery {
    /// A change to record.
    Change(Change),
    /// A delivery that is not a change (unmapped event, deployment not yet
    /// successful, merge request not merged), with the reason.
    Ignored(String),
}

/// Translate a GitLab webhook delivery.
pub fn parse(event: &str, body: &[u8]) -> Result<Delivery, Error> {
    match event {
        "Deployment Hook" => deployment(event, body),
        "Feature Flag Hook" => feature_flag(event, body),
        "Release Hook" => release(event, body),
        "Merge Request Hook" => merge_request(event, body),
        other => Ok(Delivery::Ignored(format!("event {other} is not a change"))),
    }
}

fn decode<T: serde::de::DeserializeOwned>(event: &str, body: &[u8]) -> Result<T, Error> {
    serde_json::from_slice(body).map_err(|source| Error::Shape {
        event: event.to_owned(),
        source,
    })
}

fn component(project: &Project, event: &str) -> Result<String, Error> {
    project
        .component()
        .ok_or_else(|| Error::NoProject(event.to_owned()))
}

fn push_by(summary: &mut String, user: &User) {
    if let Some(u) = user.label() {
        summary.push_str(" by ");
        summary.push_str(u);
    }
}

fn deployment(event: &str, body: &[u8]) -> Result<Delivery, Error> {
    let d: Deployment = decode(event, body)?;
    let status = d.status.as_deref().unwrap_or("");
    if status != "success" {
        return Ok(Delivery::Ignored(format!("deployment status {status}")));
    }
    let component = component(&d.project, event)?;
    let mut summary = component.clone();
    if let Some(sha) = &d.short_sha {
        summary.push(' ');
        summary.push_str(sha);
    }
    if let Some(env) = &d.environment {
        summary.push_str(" to ");
        summary.push_str(env);
    }
    if let Some(t) = d
        .commit_title
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
    {
        summary.push_str(": ");
        summary.push_str(t);
    }
    push_by(&mut summary, &d.user);
    Ok(Delivery::Change(Change {
        at: d.status_changed_at.as_deref().and_then(normalise_time),
        kind: "deploy".into(),
        component: Some(component),
        summary,
        source: Some("gitlab".into()),
        url: d.deployable_url,
    }))
}

fn feature_flag(event: &str, body: &[u8]) -> Result<Delivery, Error> {
    let f: FeatureFlag = decode(event, body)?;
    let component = component(&f.project, event)?;
    let name = f
        .object_attributes
        .name
        .as_deref()
        .unwrap_or("feature flag");
    let state = match f.object_attributes.active {
        Some(true) => "enabled",
        Some(false) => "disabled",
        None => "changed",
    };
    let mut summary = format!("flag {name} {state}");
    push_by(&mut summary, &f.user);
    Ok(Delivery::Change(Change {
        at: None,
        kind: "flag".into(),
        component: Some(component),
        summary,
        source: Some("gitlab".into()),
        url: f.project.web_url.map(|w| format!("{w}/-/feature_flags")),
    }))
}

fn release(event: &str, body: &[u8]) -> Result<Delivery, Error> {
    let r: Release = decode(event, body)?;
    if let Some(a) = r.action.as_deref()
        && a != "create"
    {
        return Ok(Delivery::Ignored(format!("release action {a}")));
    }
    let component = component(&r.project, event)?;
    let label = r.tag.as_deref().or(r.name.as_deref()).unwrap_or("release");
    Ok(Delivery::Change(Change {
        at: r
            .released_at
            .as_deref()
            .or(r.created_at.as_deref())
            .and_then(normalise_time),
        kind: "release".into(),
        component: Some(component),
        summary: format!("release {label}"),
        source: Some("gitlab".into()),
        url: r.url,
    }))
}

fn merge_request(event: &str, body: &[u8]) -> Result<Delivery, Error> {
    use std::fmt::Write;
    let m: MergeRequest = decode(event, body)?;
    let action = m.object_attributes.action.as_deref().unwrap_or("");
    if action != "merge" {
        return Ok(Delivery::Ignored(format!("merge request action {action}")));
    }
    let component = component(&m.project, event)?;
    let a = &m.object_attributes;
    let mut summary = String::from("merged");
    if let Some(iid) = a.iid {
        let _ = write!(summary, " !{iid}");
    }
    if let Some(t) = a.title.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
        summary.push(' ');
        summary.push_str(t);
    }
    if let Some(b) = &a.target_branch {
        summary.push_str(" into ");
        summary.push_str(b);
    }
    if let Some(sha) = a.merge_commit_sha.as_deref().filter(|s| s.len() >= 7) {
        summary.push_str(" (");
        summary.push_str(&sha[..7]);
        summary.push(')');
    }
    push_by(&mut summary, &m.user);
    Ok(Delivery::Change(Change {
        at: a
            .merged_at
            .as_deref()
            .or(a.updated_at.as_deref())
            .and_then(normalise_time),
        kind: "merge".into(),
        component: Some(component),
        summary,
        source: Some("gitlab".into()),
        url: a.url.clone(),
    }))
}

/// GitLab's timestamps to RFC 3339 (UTC, seconds). `None` when unparsable,
/// which lets the log fall back to receipt time.
pub fn normalise_time(raw: &str) -> Option<String> {
    let raw = raw.trim();
    let ts: Timestamp = if let Ok(t) = raw.parse::<Timestamp>() {
        t
    } else if let Some(utc) = raw.strip_suffix(" UTC") {
        strtime::parse("%Y-%m-%d %H:%M:%S", utc)
            .ok()?
            .to_datetime()
            .ok()?
            .to_zoned(jiff::tz::TimeZone::UTC)
            .ok()?
            .timestamp()
    } else {
        strtime::parse("%Y-%m-%d %H:%M:%S %z", raw)
            .ok()?
            .to_timestamp()
            .ok()?
    };
    Some(ts.strftime("%Y-%m-%dT%H:%M:%SZ").to_string())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    const DEPLOYMENT: &str = include_str!("../../examples/changes/gitlab-deployment.json");
    const FLAG: &str = include_str!("../../examples/changes/gitlab-feature-flag.json");
    const RELEASE: &str = include_str!("../../examples/changes/gitlab-release.json");
    const MERGE: &str = include_str!("../../examples/changes/gitlab-merge-request.json");

    fn change(d: Delivery) -> Change {
        match d {
            Delivery::Change(c) => c,
            Delivery::Ignored(r) => panic!("ignored: {r}"),
        }
    }

    #[test]
    fn successful_deployment_is_a_deploy_change() {
        let c = change(parse("Deployment Hook", DEPLOYMENT.as_bytes()).unwrap());
        assert_eq!(c.kind, "deploy");
        assert_eq!(c.component.as_deref(), Some("checkout-api"));
        assert_eq!(c.at.as_deref(), Some("2026-09-21T09:50:00Z")); // 11:50 +0200
        assert_eq!(
            c.summary,
            "checkout-api 9fceb02d to production: Raise memory limit to 768Mi by alice"
        );
        assert_eq!(
            c.url.as_deref(),
            Some("https://gitlab.example.com/shop/checkout-api/-/jobs/796")
        );

        let running = DEPLOYMENT.replace("\"success\"", "\"running\"");
        assert_eq!(
            parse("Deployment Hook", running.as_bytes()).unwrap(),
            Delivery::Ignored("deployment status running".into())
        );
    }

    #[test]
    fn flag_release_and_merged_merge_request_map_and_others_are_ignored() {
        let f = change(parse("Feature Flag Hook", FLAG.as_bytes()).unwrap());
        assert_eq!(
            (f.kind.as_str(), f.summary.as_str()),
            ("flag", "flag new-checkout enabled by alice")
        );
        assert_eq!(f.component.as_deref(), Some("checkout-api"));

        let r = change(parse("Release Hook", RELEASE.as_bytes()).unwrap());
        assert_eq!(
            (r.kind.as_str(), r.summary.as_str()),
            ("release", "release v2.31.0")
        );
        assert_eq!(r.at.as_deref(), Some("2026-09-21T11:40:12Z"));

        let m = change(parse("Merge Request Hook", MERGE.as_bytes()).unwrap());
        assert_eq!(m.kind, "merge");
        assert_eq!(
            m.summary,
            "merged !412 Raise memory limit to 768Mi into main (9fceb02) by alice"
        );
        assert_eq!(m.at.as_deref(), Some("2026-09-21T11:41:03Z"));

        let updated = MERGE.replace("\"action\": \"merge\"", "\"action\": \"update\"");
        assert!(matches!(
            parse("Merge Request Hook", updated.as_bytes()).unwrap(),
            Delivery::Ignored(_)
        ));
        assert!(matches!(
            parse("Push Hook", b"{}").unwrap(),
            Delivery::Ignored(r) if r.contains("Push Hook")
        ));
        assert!(parse("Deployment Hook", b"[1,2]").is_err());
    }

    #[test]
    fn gitlab_timestamps_normalise_to_rfc3339_utc() {
        assert_eq!(
            normalise_time("2021-04-28 21:50:00 +0200").as_deref(),
            Some("2021-04-28T19:50:00Z")
        );
        assert_eq!(
            normalise_time("2020-11-02 12:55:12 UTC").as_deref(),
            Some("2020-11-02T12:55:12Z")
        );
        assert_eq!(
            normalise_time("2013-12-03T17:15:43Z").as_deref(),
            Some("2013-12-03T17:15:43Z")
        );
        assert_eq!(normalise_time("yesterday"), None);
    }
}
