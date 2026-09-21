//! Webhook receiver: `POST /webhooks/incidentio`, and the change feed on
//! `POST /changes` when a token is configured.
//!
//! Verifies the Svix signature on the raw body, deduplicates on `webhook-id`
//! (Svix resends with the same id), acknowledges with 202 immediately, and
//! runs the triage in a background task. incident.io retries non-2xx for 24
//! hours, so the endpoint must only fail on requests it truly cannot accept:
//! bad signature (401) or an unparseable body (400).
//!
//! The change feed takes one [`Change`] or an array of them, authenticated
//! by a bearer token, and answers 202 with the count stored. `GET /changes`
//! with the same token lists what the replica holds.

use std::collections::{HashSet, VecDeque};
use std::sync::Arc;

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::header::AUTHORIZATION;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use jiff::Timestamp;
use serde::Deserialize;
use tokio::sync::Mutex;

use crate::changes::{Change, FeedToken};
use crate::incidentio::webhook::{Event, SignatureHeaders, VerifyError, WebhookSecret, verify_now};
use crate::incidentio::{Outcome, Triager};

/// How many recent `webhook-id`s to remember for idempotency.
const SEEN_CAPACITY: usize = 4096;

/// Shared state for the receiver.
pub struct AppState {
    /// Signing secret. `None` only when verification was explicitly disabled.
    pub secret: Option<WebhookSecret>,
    /// The flow to run.
    pub triager: Triager,
    /// Optional sink for outcomes (tests, metrics). Errors are logged.
    pub on_outcome: Option<tokio::sync::mpsc::UnboundedSender<Result<Outcome, String>>>,
    /// Token for the change feed. `None` leaves `/changes` unrouted.
    pub changes_token: Option<FeedToken>,
    seen: Mutex<Seen>,
}

struct Seen {
    set: HashSet<String>,
    order: VecDeque<String>,
}

impl Seen {
    fn insert(&mut self, id: String) -> bool {
        if !self.set.insert(id.clone()) {
            return false;
        }
        self.order.push_back(id);
        while self.order.len() > SEEN_CAPACITY
            && let Some(old) = self.order.pop_front()
        {
            self.set.remove(&old);
        }
        true
    }
}

impl AppState {
    /// Build state. Pass `None` for `secret` only for local development; the
    /// server logs a loud warning and accepts unsigned deliveries.
    pub fn new(secret: Option<WebhookSecret>, triager: Triager) -> Self {
        Self {
            secret,
            triager,
            on_outcome: None,
            changes_token: None,
            seen: Mutex::new(Seen {
                set: HashSet::new(),
                order: VecDeque::new(),
            }),
        }
    }
}

/// Build the router.
pub fn router(state: Arc<AppState>) -> Router {
    if state.secret.is_none() {
        tracing::warn!(
            "webhook signature verification is DISABLED; never run this way in production"
        );
    }
    let mut router = Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/webhooks/incidentio", post(receive));
    if state.changes_token.is_some() {
        if state.triager.changes.is_none() {
            tracing::warn!(
                "change feed token set but the triager has no change log; changes will be refused"
            );
        }
        router = router.route("/changes", post(post_changes).get(get_changes));
    }
    router.with_state(state)
}

#[derive(Deserialize)]
#[serde(untagged)]
enum OneOrMany {
    One(Change),
    Many(Vec<Change>),
}

fn authorized(state: &AppState, headers: &HeaderMap) -> bool {
    state
        .changes_token
        .as_ref()
        .is_some_and(|t| t.accepts(headers.get(AUTHORIZATION).and_then(|v| v.to_str().ok())))
}

fn unauthorized() -> Response {
    (StatusCode::UNAUTHORIZED, "missing or wrong bearer token").into_response()
}

async fn post_changes(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    let Some(log) = &state.triager.changes else {
        return (StatusCode::SERVICE_UNAVAILABLE, "change feed disabled").into_response();
    };
    let parsed: OneOrMany = match serde_json::from_slice(&body) {
        Ok(p) => p,
        Err(e) => {
            return (StatusCode::BAD_REQUEST, format!("invalid change: {e}")).into_response();
        }
    };
    let changes = match parsed {
        OneOrMany::One(c) => vec![c],
        OneOrMany::Many(v) => v,
    };
    if changes
        .iter()
        .any(|c| c.kind.trim().is_empty() || c.summary.trim().is_empty())
    {
        return (
            StatusCode::BAD_REQUEST,
            "kind and summary must not be empty",
        )
            .into_response();
    }
    let now = Timestamp::now();
    let n = changes.len();
    for c in changes {
        let stored = log.record(c, now);
        tracing::info!(kind = %stored.kind, component = ?stored.component, summary = %stored.summary, "change recorded");
    }
    (
        StatusCode::ACCEPTED,
        axum::Json(serde_json::json!({ "recorded": n, "held": log.len() })),
    )
        .into_response()
}

async fn get_changes(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    let Some(log) = &state.triager.changes else {
        return (StatusCode::SERVICE_UNAVAILABLE, "change feed disabled").into_response();
    };
    axum::Json(log.all()).into_response()
}

async fn receive(State(state): State<Arc<AppState>>, headers: HeaderMap, body: Bytes) -> Response {
    if let Some(secret) = &state.secret {
        let sig = match SignatureHeaders::from_headers(&headers) {
            Ok(h) => h,
            Err(e) => return reject(StatusCode::UNAUTHORIZED, &e),
        };
        if let Err(e) = verify_now(secret, &sig, &body) {
            tracing::warn!(error = %e, webhook_id = %sig.id, "rejected webhook");
            return reject(StatusCode::UNAUTHORIZED, &e);
        }
    }

    let webhook_id = headers
        .get("webhook-id")
        .or_else(|| headers.get("svix-id"))
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_owned();
    if !webhook_id.is_empty() && !state.seen.lock().await.insert(webhook_id.clone()) {
        tracing::info!(%webhook_id, "duplicate delivery ignored");
        return StatusCode::OK.into_response();
    }

    let event = match Event::parse(&body) {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!(error = %e, %webhook_id, "unparseable webhook body");
            return (StatusCode::BAD_REQUEST, e.to_string()).into_response();
        }
    };

    match event {
        Event::AlertCreated(alert) => {
            tracing::info!(alert_id = %alert.id, title = %alert.title, %webhook_id, "alert created; triaging");
            let state = Arc::clone(&state);
            tokio::spawn(async move {
                let result = state.triager.triage_alert_by_id(&alert.id).await;
                if let Err(e) = &result {
                    tracing::error!(alert_id = %alert.id, error = %e, "triage failed");
                }
                if let Some(tx) = &state.on_outcome {
                    let _ = tx.send(result.map_err(|e| e.to_string()));
                }
            });
            StatusCode::ACCEPTED.into_response()
        }
        other => {
            tracing::debug!(event_type = other.event_type(), %webhook_id, "event ignored");
            StatusCode::OK.into_response()
        }
    }
}

fn reject(status: StatusCode, err: &VerifyError) -> Response {
    (status, err.to_string()).into_response()
}
