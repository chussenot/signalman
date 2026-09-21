//! Argo CD adapter: an `Application` object, as Argo CD Notifications sends
//! it with the one-line template `{{ toJson .app }}`, becomes a deploy
//! change. Optionally wrapped as `{"app": …, "context": {"argocdUrl": …}}`
//! so the change carries a link.
//!
//! Only the fields read are modelled; everything else is ignored, because
//! the Application schema grows with every Argo CD release.

use serde::Deserialize;

use super::Change;

/// The posted body: either a bare `Application` or `{app, context}`.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum Body {
    Wrapped {
        app: Application,
        #[serde(default)]
        context: Option<Context>,
    },
    Bare(Application),
}

#[derive(Debug, Default, Deserialize)]
struct Context {
    #[serde(default, rename = "argocdUrl")]
    argocd_url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Application {
    metadata: Metadata,
    #[serde(default)]
    spec: Spec,
    #[serde(default)]
    status: Status,
}

#[derive(Debug, Deserialize)]
struct Metadata {
    name: String,
}

#[derive(Debug, Default, Deserialize)]
struct Spec {
    #[serde(default)]
    destination: Destination,
    #[serde(default)]
    project: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct Destination {
    #[serde(default)]
    namespace: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct Status {
    #[serde(default)]
    history: Vec<History>,
    #[serde(default, rename = "operationState")]
    operation_state: Option<OperationState>,
    #[serde(default)]
    sync: Option<SyncStatus>,
    #[serde(default)]
    summary: Option<Summary>,
}

#[derive(Debug, Default, Deserialize)]
struct History {
    #[serde(default)]
    revision: Option<String>,
    #[serde(default, rename = "deployedAt")]
    deployed_at: Option<String>,
    #[serde(default, rename = "initiatedBy")]
    initiated_by: Option<InitiatedBy>,
}

#[derive(Debug, Default, Deserialize)]
struct InitiatedBy {
    #[serde(default)]
    username: Option<String>,
    #[serde(default)]
    automated: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
struct OperationState {
    #[serde(default)]
    phase: Option<String>,
    #[serde(default, rename = "finishedAt")]
    finished_at: Option<String>,
    #[serde(default, rename = "syncResult")]
    sync_result: Option<SyncResult>,
}

#[derive(Debug, Default, Deserialize)]
struct SyncResult {
    #[serde(default)]
    revision: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct SyncStatus {
    #[serde(default)]
    revision: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct Summary {
    #[serde(default)]
    images: Vec<String>,
}

/// Why a body was not turned into a change.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Not an Application.
    #[error("not an Argo CD Application: {0}")]
    Shape(#[from] serde_json::Error),
    /// A sync that did not succeed is not a change that happened.
    #[error("operation phase is {0}, not Succeeded")]
    NotSucceeded(String),
}

/// Translate an Argo CD body into a change. Syncs whose `operationState.phase`
/// is present and not `Succeeded` are refused: a failed sync changed nothing.
pub fn parse(body: &[u8]) -> Result<Change, Error> {
    let (app, context) = match serde_json::from_slice::<Body>(body)? {
        Body::Wrapped { app, context } => (app, context),
        Body::Bare(app) => (app, None),
    };
    if let Some(phase) = app
        .status
        .operation_state
        .as_ref()
        .and_then(|o| o.phase.as_deref())
        && phase != "Succeeded"
    {
        return Err(Error::NotSucceeded(phase.to_owned()));
    }
    let last = app.status.history.last();
    let revision = last
        .and_then(|h| h.revision.as_deref())
        .or_else(|| {
            app.status
                .operation_state
                .as_ref()
                .and_then(|o| o.sync_result.as_ref())
                .and_then(|r| r.revision.as_deref())
        })
        .or_else(|| app.status.sync.as_ref().and_then(|s| s.revision.as_deref()));
    let at = last.and_then(|h| h.deployed_at.clone()).or_else(|| {
        app.status
            .operation_state
            .as_ref()
            .and_then(|o| o.finished_at.clone())
    });

    let mut summary = format!("{} synced", app.metadata.name);
    if let Some(r) = revision {
        summary.push_str(" to ");
        summary.push_str(short(r));
    }
    if let Some(ns) = &app.spec.destination.namespace {
        summary.push_str(" in ");
        summary.push_str(ns);
    }
    if let Some(images) = app.status.summary.as_ref().map(|s| &s.images)
        && !images.is_empty()
    {
        summary.push_str(", images ");
        summary.push_str(
            &images
                .iter()
                .map(|i| short_image(i))
                .collect::<Vec<_>>()
                .join(", "),
        );
    }
    match last.and_then(|h| h.initiated_by.as_ref()) {
        Some(InitiatedBy {
            username: Some(u), ..
        }) if !u.is_empty() => {
            summary.push_str(" by ");
            summary.push_str(u);
        }
        Some(InitiatedBy {
            automated: Some(true),
            ..
        }) => summary.push_str(" by auto-sync"),
        _ => {}
    }
    if let Some(p) = &app.spec.project
        && p != "default"
    {
        summary.push_str(" (project ");
        summary.push_str(p);
        summary.push(')');
    }

    Ok(Change {
        at,
        kind: "deploy".into(),
        component: Some(app.metadata.name.clone()),
        summary,
        source: Some("argocd".into()),
        url: context.and_then(|c| c.argocd_url).map(|u| {
            format!(
                "{}/applications/{}",
                u.trim_end_matches('/'),
                app.metadata.name
            )
        }),
    })
}

/// First seven characters of a commit SHA; tags and short forms unchanged.
fn short(revision: &str) -> &str {
    if revision.len() >= 40 && revision.chars().all(|c| c.is_ascii_hexdigit()) {
        &revision[..7]
    } else {
        revision
    }
}

/// `registry/repo/name:tag` becomes `name:tag`.
fn short_image(image: &str) -> &str {
    image.rsplit('/').next().unwrap_or(image)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    const APP: &str = include_str!("../../examples/changes/argocd-application.json");

    #[test]
    fn application_becomes_a_deploy_change_with_revision_namespace_images_and_actor() {
        let c = parse(APP.as_bytes()).unwrap();
        assert_eq!(c.kind, "deploy");
        assert_eq!(c.component.as_deref(), Some("checkout-api"));
        assert_eq!(c.at.as_deref(), Some("2026-09-21T11:50:12Z"));
        assert_eq!(
            c.summary,
            "checkout-api synced to 9fceb02 in shop, images checkout-api:2.31.0 by alice"
        );
        assert_eq!(c.source.as_deref(), Some("argocd"));
        assert_eq!(
            c.url.as_deref(),
            Some("https://argocd.example.com/applications/checkout-api")
        );
    }

    #[test]
    fn bare_application_without_context_has_no_url_and_failed_syncs_are_refused() {
        let v: serde_json::Value = serde_json::from_str(APP).unwrap();
        let bare = serde_json::to_vec(&v["app"]).unwrap();
        let c = parse(&bare).unwrap();
        assert!(c.url.is_none());
        assert_eq!(c.component.as_deref(), Some("checkout-api"));

        let mut failed = v["app"].clone();
        failed["status"]["operationState"]["phase"] = serde_json::json!("Failed");
        let err = parse(&serde_json::to_vec(&failed).unwrap()).unwrap_err();
        assert!(matches!(err, Error::NotSucceeded(p) if p == "Failed"));

        assert!(parse(br#"{"kind": "Rollout"}"#).is_err());
    }
}
