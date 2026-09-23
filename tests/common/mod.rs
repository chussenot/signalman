//! Fixtures shared by the integration tests: the clients pointed at mock
//! servers, one incident.io alert with its open incident and blast radius,
//! one TypeSafe answer set, the committed outcome schema, and the bare MCP
//! client the in-process and HTTP transport tests both drive.
//!
//! The fixtures exist because four test files had grown the same `al-1` /
//! `INC-4821` / `jev-1.13.0` scene by copy, and a change to one shape (a
//! new attribute on the alert, a new question in the answers) had to be
//! made four times or silently diverge. What still differs between tests is
//! a parameter here (`SystemOne`), not a second copy.
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use rmcp::ClientHandler;
use rmcp::model::CallToolRequestParams;
use serde_json::{Value, json};
use signalman::incidentio::Triager;
use signalman::{Client, RetryPolicy, backstage, incidentio};
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

// ---------------------------------------------------------------------------
// Clients
// ---------------------------------------------------------------------------

/// A TypeSafe client for a mock, failing once rather than retrying.
pub fn typesafe_client(base_url: &str) -> Client {
    Client::builder()
        .api_key("ts")
        .base_url(base_url)
        .retry(RetryPolicy::none())
        .build()
        .unwrap()
}

/// An incident.io client for a mock, failing once rather than retrying.
pub fn incidentio_client(base_url: &str) -> incidentio::Client {
    incidentio::Client::builder()
        .api_key("io")
        .base_url(base_url)
        .retry(RetryPolicy::none())
        .build()
        .unwrap()
}

/// A Backstage client for a mock, failing once rather than retrying.
pub fn backstage_client(base_url: &str) -> backstage::Client {
    backstage::Client::builder()
        .base_url(base_url)
        .retry(RetryPolicy::none())
        .build()
        .unwrap()
}

/// A `Triager` over the two mocks, with the flow's defaults.
pub fn triager(typesafe: &MockServer, incidentio: &MockServer) -> Triager {
    Triager::new(
        typesafe_client(&typesafe.uri()),
        incidentio_client(&incidentio.uri()),
    )
}

// ---------------------------------------------------------------------------
// The incident.io scene: alert al-1, incident INC-4821, blast radius
// ---------------------------------------------------------------------------

/// The alert under triage in most scenarios.
pub const ALERT_ID: &str = "al-1";
/// The one open incident offered as a duplicate candidate.
pub const INCIDENT_ID: &str = "01INC4821";
/// Its reference, the value TypeSafe chooses between.
pub const INCIDENT_REF: &str = "INC-4821";

/// `GET /v2/alerts/al-1`: a firing alert with a creation time, so the
/// outcome carries a time to qualify.
pub fn alert_al1() -> Value {
    json!({
        "id": ALERT_ID, "alert_source_id": "src-dd", "title": "HighErrorRate checkout-api",
        "description": "5xx ratio 12% for 10m", "status": "firing",
        "created_at": "2026-09-20T11:58:00Z", "attributes": [], "tags": []
    })
}

/// The live incident `INC-4821`, as the incidents list returns it.
pub fn incident_inc4821() -> Value {
    json!({
        "id": INCIDENT_ID, "reference": INCIDENT_REF, "name": "Checkout 5xx spike",
        "summary": "payments-gateway returning errors",
        "permalink": "https://app.incident.io/org/incidents/4821",
        "incident_status": { "id": "s", "name": "Active", "category": "live" },
        "mode": "standard"
    })
}

/// `al-1` as the firing-alerts list returns it (the flow drops it as
/// "this alert").
pub fn firing_al1() -> Value {
    json!({ "id": ALERT_ID, "alert_source_id": "src-dd", "title": "HighErrorRate checkout-api",
            "status": "firing", "attributes": [], "tags": [], "created_at": "2026-09-20T11:58:00Z" })
}

/// Another alert firing in the window, on a different component.
pub fn firing_al9() -> Value {
    json!({ "id": "al-9", "alert_source_id": "src-dd", "title": "HighLatency payments-gateway",
            "status": "firing", "attributes": [], "tags": [], "created_at": "2026-09-20T11:55:00Z" })
}

/// Mount `GET /v2/alerts/al-1` answering [`alert_al1`].
pub async fn mount_alert_al1(incidentio: &MockServer) {
    Mock::given(method("GET"))
        .and(path(format!("/v2/alerts/{ALERT_ID}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "alert": alert_al1() })))
        .mount(incidentio)
        .await;
}

/// Mount `GET /v2/incidents` answering `incidents`, one page.
pub async fn mount_open_incidents(incidentio: &MockServer, incidents: Vec<Value>) {
    Mock::given(method("GET"))
        .and(path("/v2/incidents"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "incidents": incidents, "pagination_meta": { "page_size": 40 }
        })))
        .mount(incidentio)
        .await;
}

/// Mount `GET /v2/alerts?status[one_of]=firing` answering `alerts`.
pub async fn mount_firing_alerts(incidentio: &MockServer, alerts: Vec<Value>) {
    Mock::given(method("GET"))
        .and(path("/v2/alerts"))
        .and(query_param("status[one_of]", "firing"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "alerts": alerts, "pagination_meta": { "page_size": 50 }
        })))
        .mount(incidentio)
        .await;
}

/// The whole read-side scene: `al-1`, `INC-4821` open, `al-9` also firing.
pub async fn mount_scene(incidentio: &MockServer) {
    mount_alert_al1(incidentio).await;
    mount_open_incidents(incidentio, vec![incident_inc4821()]).await;
    mount_firing_alerts(incidentio, vec![firing_al1(), firing_al9()]).await;
}

/// Every write endpoint, mounted and expected zero times: a write that
/// leaks through a dry run fails the test with wiremock's own message
/// naming the endpoint, rather than a generic upstream error.
pub async fn mount_no_writes(incidentio: &MockServer) {
    for (m, p) in [
        ("POST", "/v2/alerts/al-1/actions/add_tags"),
        ("POST", "/v2/incident_alerts"),
        ("POST", "/v1/alert_notes"),
        ("PUT", "/v1/alert_notes/note-1"),
    ] {
        Mock::given(method(m))
            .and(path(p))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
            .expect(0)
            .mount(incidentio)
            .await;
    }
}

// ---------------------------------------------------------------------------
// The TypeSafe answer set
// ---------------------------------------------------------------------------

/// One `POST /v1/systemone` answer: the `application` team at 0.8, a major
/// impact, actionable, and a dedup choice that decides between `page`
/// (`"none"`) and `attach_to_incident` (`INCIDENT_REF`). The probabilities
/// follow the choice so the answer stays self-consistent.
#[derive(Debug, Clone)]
pub struct SystemOne {
    /// `"none"` or [`INCIDENT_REF`].
    pub duplicate_choice: &'static str,
    /// Confidence reported on the dedup answer.
    pub duplicate_confidence: f64,
    /// Confidence reported on the owner answer.
    pub owner_confidence: f64,
    /// Confidence reported on the impact answer.
    pub impact_confidence: f64,
}

impl Default for SystemOne {
    fn default() -> Self {
        Self {
            duplicate_choice: "none",
            duplicate_confidence: 0.8,
            owner_confidence: 0.75,
            impact_confidence: 0.9,
        }
    }
}

impl SystemOne {
    /// The same answers deciding `attach_to_incident`.
    pub fn attaching(mut self) -> Self {
        self.duplicate_choice = INCIDENT_REF;
        self
    }

    /// The response body.
    pub fn body(&self) -> Value {
        let attach = self.duplicate_choice == INCIDENT_REF;
        json!({
            "model": "jev-1.13.0",
            "answers": {
                "owner": { "type": "choice", "choice": "application",
                           "probabilities": { "application": 0.8, "platform": 0.2 },
                           "confidence": self.owner_confidence },
                "impact": { "type": "score", "score": 2.0, "legend": { "0": "a", "1": "b", "2": "c", "3": "d" },
                            "probabilities": { "0": 0.0, "1": 0.0, "2": 1.0, "3": 0.0 },
                            "confidence": self.impact_confidence },
                "actionable": { "type": "noul", "noul": 0.95 },
                "duplicate_of": { "type": "choice", "choice": self.duplicate_choice,
                                  "probabilities": { INCIDENT_REF: if attach { 0.9 } else { 0.1 },
                                                     "none": if attach { 0.1 } else { 0.9 } },
                                  "confidence": self.duplicate_confidence },
                "caused_by_change": { "type": "noul", "noul": 0.2 }
            },
            "usage": { "input_tokens": 500, "output_tokens": 30 }
        })
    }

    /// Mount it on the TypeSafe mock for every `POST /v1/systemone`.
    pub async fn mount(&self, typesafe: &MockServer) {
        Mock::given(method("POST"))
            .and(path("/v1/systemone"))
            .respond_with(ResponseTemplate::new(200).set_body_json(self.body()))
            .mount(typesafe)
            .await;
    }
}

// ---------------------------------------------------------------------------
// The outcome contract
// ---------------------------------------------------------------------------

/// The committed schema, the one consumers are pointed at.
pub const SCHEMA_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/docs/schema/outcome.v1.json");

/// The committed schema as JSON; panics with the regeneration command when
/// the file is missing.
pub fn committed_schema() -> Value {
    let raw = std::fs::read_to_string(SCHEMA_PATH).unwrap_or_else(|e| {
        panic!("{SCHEMA_PATH} is missing ({e}); run `mise run schema` to generate it")
    });
    serde_json::from_str(&raw).expect("the committed schema is valid JSON")
}

// ---------------------------------------------------------------------------
// MCP client
// ---------------------------------------------------------------------------

/// A client that answers nothing on its own: every test drives the
/// connection by calling tools, never by receiving a server-initiated
/// request.
#[derive(Debug, Clone, Default)]
pub struct DummyClient;

impl ClientHandler for DummyClient {}

/// A `tools/call` request; `args` must be a JSON object or `null`.
pub fn call(name: &'static str, args: Value) -> CallToolRequestParams {
    let object = match args {
        Value::Object(o) => o,
        Value::Null => serde_json::Map::new(),
        other => panic!("tool arguments must be a JSON object, got {other}"),
    };
    CallToolRequestParams::new(name).with_arguments(object)
}
