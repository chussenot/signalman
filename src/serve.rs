//! Webhook receiver: `POST /webhooks/incidentio`, and the change feed on
//! `POST /changes` when a token is configured.
//!
//! Verifies the Svix signature on the raw body, deduplicates on `webhook-id`
//! (Svix resends with the same id), acknowledges with 202 immediately, and
//! runs the triage in a background task. incident.io retries non-2xx for 24
//! hours, so the endpoint must only fail on requests it truly cannot accept:
//! bad signature (401), an unparseable body (400), or more work than this
//! replica will take on (503 with `Retry-After`, so the hub's retry becomes
//! the backpressure).
//!
//! Work is bounded twice. At most [`Limits::max_concurrent`] triages run at
//! once and at most [`Limits::max_queued`] wait for a slot; a delivery
//! beyond that is refused before it is marked seen, so the retry is
//! processed. Each triage has a deadline, [`Limits::timeout`], covering every
//! upstream call; a triage that overruns is dropped and reported as an
//! error, and its alert keeps the tags it had.
//!
//! The change feed takes one [`Change`] or an array of them, authenticated
//! by a bearer token, and answers 202 with the count stored. `GET /changes`
//! with the same token lists what the replica holds.

use std::collections::{HashSet, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::header::AUTHORIZATION;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use jiff::Timestamp;
use serde::Deserialize;
use tokio::sync::{Mutex, Semaphore};
use tracing::Instrument;

use crate::changes::gitlab::{self, Delivery};
use crate::changes::{Change, ChangeLog, FeedToken, argocd};
use crate::incidentio::webhook::{Event, SignatureHeaders, VerifyError, WebhookSecret, verify_now};
use crate::incidentio::{Outcome, Triager};

/// How many recent `webhook-id`s to remember for idempotency.
const SEEN_CAPACITY: usize = 4096;

/// Triages running at once. Each costs a handful of incident.io calls, and
/// the incidents list is limited to 60 a minute, so this is not a CPU bound.
pub const DEFAULT_MAX_CONCURRENT: usize = 8;

/// Triages waiting for a slot before deliveries are refused.
pub const DEFAULT_MAX_QUEUED: usize = 64;

/// Deadline for one triage, all upstream calls and retries included. The
/// clients' own timeouts bound each call; this bounds the sum.
pub const DEFAULT_TRIAGE_TIMEOUT: Duration = Duration::from_secs(60);

/// Seconds suggested to the sender on a 503.
const RETRY_AFTER_SECONDS: u64 = 30;

/// Bounds on the background work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Triages running at once (at least 1).
    pub max_concurrent: usize,
    /// Triages waiting for a slot; beyond it, 503.
    pub max_queued: usize,
    /// Deadline per triage.
    pub timeout: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_concurrent: DEFAULT_MAX_CONCURRENT,
            max_queued: DEFAULT_MAX_QUEUED,
            timeout: DEFAULT_TRIAGE_TIMEOUT,
        }
    }
}

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
    /// The bounds in force.
    pub limits: Limits,
    seen: Mutex<Seen>,
    /// One permit per triage admitted (running or waiting).
    admission: Arc<Semaphore>,
    /// One permit per triage running.
    running: Arc<Semaphore>,
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
        Self::with_limits(secret, triager, Limits::default())
    }

    /// Build state with explicit bounds.
    pub fn with_limits(secret: Option<WebhookSecret>, triager: Triager, limits: Limits) -> Self {
        let concurrent = limits.max_concurrent.max(1);
        Self {
            secret,
            triager,
            on_outcome: None,
            changes_token: None,
            limits,
            seen: Mutex::new(Seen {
                set: HashSet::new(),
                order: VecDeque::new(),
            }),
            admission: Arc::new(Semaphore::new(concurrent + limits.max_queued)),
            running: Arc::new(Semaphore::new(concurrent)),
        }
    }

    /// Triages currently running.
    pub fn running(&self) -> usize {
        self.limits
            .max_concurrent
            .max(1)
            .saturating_sub(self.running.available_permits())
    }

    /// Triages admitted and not yet finished (running plus waiting).
    pub fn admitted(&self) -> usize {
        (self.limits.max_concurrent.max(1) + self.limits.max_queued)
            .saturating_sub(self.admission.available_permits())
    }
}

/// Build the router.
pub fn router(state: Arc<AppState>) -> Router {
    crate::telemetry::observe_inflight(&state);
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
        router = router
            .route("/changes", post(post_changes).get(get_changes))
            .route("/changes/argocd", post(post_argocd))
            .route("/changes/gitlab", post(post_gitlab));
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

/// Record one translated change and answer 202.
fn accepted(log: &ChangeLog, change: Change, source: &str) -> Response {
    let stored = log.record(change, Timestamp::now());
    tracing::info!(source, kind = %stored.kind, component = ?stored.component, summary = %stored.summary, "change recorded");
    (
        StatusCode::ACCEPTED,
        axum::Json(serde_json::json!({ "recorded": 1, "held": log.len(), "change": stored })),
    )
        .into_response()
}

/// A delivery that is not a change: 200 so the sender does not retry.
fn ignored(reason: &str) -> Response {
    tracing::debug!(reason, "change delivery ignored");
    (
        StatusCode::OK,
        axum::Json(serde_json::json!({ "recorded": 0, "ignored": reason })),
    )
        .into_response()
}

/// Argo CD Notifications posting the Application object.
async fn post_argocd(
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
    match argocd::parse(&body) {
        Ok(change) => accepted(log, change, "argocd"),
        Err(argocd::Error::NotSucceeded(phase)) => ignored(&format!("sync phase {phase}")),
        Err(e) => (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
    }
}

/// GitLab project webhooks, authenticated by `X-Gitlab-Token`.
async fn post_gitlab(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let token = headers
        .get(gitlab::TOKEN_HEADER)
        .and_then(|v| v.to_str().ok());
    if !state
        .changes_token
        .as_ref()
        .is_some_and(|t| t.accepts_raw(token))
    {
        return (StatusCode::UNAUTHORIZED, "missing or wrong X-Gitlab-Token").into_response();
    }
    let Some(log) = &state.triager.changes else {
        return (StatusCode::SERVICE_UNAVAILABLE, "change feed disabled").into_response();
    };
    let event = headers
        .get(gitlab::EVENT_HEADER)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    match gitlab::parse(event, &body) {
        Ok(Delivery::Change(change)) => accepted(log, change, "gitlab"),
        Ok(Delivery::Ignored(reason)) => ignored(&reason),
        Err(e) => (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
    }
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

/// The webhook handler, as one span per delivery (`webhook.receive`) whose
/// `result` field says what became of it, mirrored in the
/// `signalman.webhook.deliveries` counter.
async fn receive(State(state): State<Arc<AppState>>, headers: HeaderMap, body: Bytes) -> Response {
    let span = tracing::info_span!(
        "webhook.receive",
        webhook_id = tracing::field::Empty,
        event_type = tracing::field::Empty,
        result = tracing::field::Empty
    );
    receive_inner(state, headers, body).instrument(span).await
}

/// Record how the delivery ended, on the span and in the counter.
fn delivered(result: &'static str) {
    tracing::Span::current().record("result", result);
    crate::telemetry::record_webhook_delivery(result);
}

#[allow(clippy::too_many_lines)] // verify, dedup, parse, admit, spawn: one linear path per delivery
async fn receive_inner(state: Arc<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    if let Some(secret) = &state.secret {
        let sig = match SignatureHeaders::from_headers(&headers) {
            Ok(h) => h,
            Err(e) => {
                delivered("rejected");
                return reject(StatusCode::UNAUTHORIZED, &e);
            }
        };
        if let Err(e) = verify_now(secret, &sig, &body) {
            tracing::warn!(error = %e, webhook_id = %sig.id, "rejected webhook");
            delivered("rejected");
            return reject(StatusCode::UNAUTHORIZED, &e);
        }
    }

    let webhook_id = headers
        .get("webhook-id")
        .or_else(|| headers.get("svix-id"))
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_owned();
    tracing::Span::current().record("webhook_id", webhook_id.as_str());
    if !webhook_id.is_empty() && state.seen.lock().await.set.contains(&webhook_id) {
        tracing::info!(%webhook_id, "duplicate delivery ignored");
        delivered("duplicate");
        return StatusCode::OK.into_response();
    }

    let event = match Event::parse(&body) {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!(error = %e, %webhook_id, "unparseable webhook body");
            delivered("invalid");
            return (StatusCode::BAD_REQUEST, e.to_string()).into_response();
        }
    };
    tracing::Span::current().record("event_type", event.event_type());

    match event {
        Event::AlertCreated(alert) => {
            // Admission first, then the seen set: a refused delivery must be
            // retried, so it must not be remembered.
            let Ok(admitted) = Arc::clone(&state.admission).try_acquire_owned() else {
                tracing::warn!(
                    alert_id = %alert.id,
                    %webhook_id,
                    running = state.running(),
                    admitted = state.admitted(),
                    "at capacity; asking incident.io to retry"
                );
                delivered("refused");
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    [(
                        axum::http::header::RETRY_AFTER,
                        RETRY_AFTER_SECONDS.to_string(),
                    )],
                    "triage capacity reached; retry later",
                )
                    .into_response();
            };
            if !webhook_id.is_empty() && !state.seen.lock().await.insert(webhook_id.clone()) {
                tracing::info!(%webhook_id, "duplicate delivery ignored");
                delivered("duplicate");
                return StatusCode::OK.into_response();
            }
            tracing::info!(alert_id = %alert.id, title = %alert.title, %webhook_id, queued = state.admitted().saturating_sub(state.running()), "alert created; triaging");
            delivered("accepted");
            let state = Arc::clone(&state);
            // The triage outlives the 202, so its span is not a child of the
            // request's: it follows from it, which OpenTelemetry renders as
            // a link between the two traces.
            let triage_span =
                tracing::info_span!("triage.background", alert_id = %alert.id, %webhook_id);
            triage_span.follows_from(tracing::Span::current().id());
            tokio::spawn(async move {
                let _admitted = admitted;
                let Ok(_running) = Arc::clone(&state.running).acquire_owned().await else {
                    return; // semaphore closed: shutting down
                };
                let timeout = state.limits.timeout;
                let result = match tokio::time::timeout(
                    timeout,
                    state.triager.triage_alert_by_id(&alert.id),
                )
                .await
                {
                    Ok(r) => r.map_err(|e| e.to_string()),
                    Err(_) => Err(format!(
                        "triage timed out after {:.1} s; the alert keeps whatever was written before the deadline",
                        timeout.as_secs_f64()
                    )),
                };
                if let Err(e) = &result {
                    tracing::error!(alert_id = %alert.id, error = %e, "triage failed");
                }
                if let Some(tx) = &state.on_outcome {
                    let _ = tx.send(result);
                }
            }.instrument(triage_span));
            StatusCode::ACCEPTED.into_response()
        }
        other => {
            tracing::debug!(event_type = other.event_type(), %webhook_id, "event ignored");
            delivered("ignored");
            StatusCode::OK.into_response()
        }
    }
}

fn reject(status: StatusCode, err: &VerifyError) -> Response {
    (status, err.to_string()).into_response()
}
