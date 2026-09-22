//! The MCP server end to end: a real client, over an in-process transport,
//! driving each tool against mock TypeSafe, incident.io and Backstage
//! servers. `qualify_alert` is checked against the committed outcome schema
//! and, separately, against a `Triager` explicitly configured to apply
//! writes — proving `mcp::Server::new` forces dry run regardless.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::too_many_lines)]

use jiff::Timestamp;
use rmcp::model::{CallToolRequestParams, ErrorCode};
use rmcp::service::ServiceError;
use rmcp::{ClientHandler, ServiceExt};
use serde_json::{Value, json};
use signalman::backstage::Enricher;
use signalman::changes::{Change, ChangeLog};
use signalman::incidentio::{Triager, WriteBack};
use signalman::mcp;
use signalman::outcome::Outcome;
use signalman::{Client, RetryPolicy};
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Serialises this file's tests. Each spins up one to three `wiremock`
/// servers on top of an MCP client/server pair; ten of those running
/// concurrently (the default test-harness parallelism, on top of every
/// other integration-test binary `cargo test` runs at the same time)
/// starves the sandbox's loopback sockets and produces a spurious 404 on
/// an otherwise-correctly-mounted mock. The fix is concurrency, not
/// retries: one test's servers are fully torn down before the next
/// starts.
static SERIALIZE: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();

async fn serialized() -> tokio::sync::MutexGuard<'static, ()> {
    SERIALIZE
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await
}

/// The committed schema `qualify_alert` results must satisfy.
const SCHEMA_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/docs/schema/outcome.v1.json");

fn committed_schema() -> Value {
    let raw = std::fs::read_to_string(SCHEMA_PATH).unwrap_or_else(|e| {
        panic!("{SCHEMA_PATH} is missing ({e}); run `mise run schema` to generate it")
    });
    serde_json::from_str(&raw).expect("the committed schema is valid JSON")
}

/// A client that answers nothing on its own: every test drives the
/// connection by calling tools, never by receiving a server-initiated
/// request.
#[derive(Debug, Clone, Default)]
struct DummyClient;

impl ClientHandler for DummyClient {}

/// Connect `server` and a fresh client over an in-process duplex pipe (no
/// stdio, no process): the same wiring `signalman mcp` sets up over real
/// stdin/stdout, minus the transport.
async fn connected(
    server: mcp::Server,
) -> rmcp::service::RunningService<rmcp::RoleClient, DummyClient> {
    let (server_io, client_io) = tokio::io::duplex(64 * 1024);
    tokio::spawn(async move {
        let running = server.serve(server_io).await.expect("server connects");
        running.waiting().await.ok();
    });
    DummyClient.serve(client_io).await.expect("client connects")
}

fn call(name: &'static str, args: Value) -> CallToolRequestParams {
    let object = match args {
        Value::Object(o) => o,
        Value::Null => serde_json::Map::new(),
        other => panic!("tool arguments must be a JSON object, got {other}"),
    };
    CallToolRequestParams::new(name).with_arguments(object)
}

/// A malformed request was refused as a protocol-level error (never a
/// tool-level one a client would render as the tool's own output): exactly
/// the distinction `rmcp::model::CallToolResult::error`'s guidance draws
/// between "the tool ran and failed" and "the request was never valid".
fn assert_invalid_params(err: ServiceError) {
    match err {
        ServiceError::McpError(e) => {
            assert_eq!(e.code, ErrorCode::INVALID_PARAMS, "{e:?}");
        }
        other => panic!("expected a protocol-level invalid_params error, got {other:?}"),
    }
}

/// incident.io alert `al-1`, one open incident (`INC-4821`) and one other
/// firing alert on `payments-gateway`, plus a TypeSafe response that pages
/// the `application` team. Backstage and the change feed are not mounted;
/// pass `changes` to attach a feed, and add a Backstage server separately
/// for `lookup_owner`.
async fn triager(
    write_back: WriteBack,
    changes: Option<ChangeLog>,
) -> (Triager, MockServer, MockServer) {
    let incidentio_srv = MockServer::start().await;
    let typesafe_srv = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/v2/alerts/al-1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "alert": {
                "id": "al-1", "alert_source_id": "src-dd", "title": "HighErrorRate checkout-api",
                "description": "5xx ratio 12% for 10m", "status": "firing",
                "created_at": "2026-09-20T11:58:00Z", "attributes": [], "tags": []
            }
        })))
        .mount(&incidentio_srv)
        .await;
    Mock::given(method("GET"))
        .and(path("/v2/incidents"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "incidents": [{
                "id": "01INC4821", "reference": "INC-4821", "name": "Checkout 5xx spike",
                "summary": "payments-gateway returning errors",
                "permalink": "https://app.incident.io/org/incidents/4821",
                "incident_status": { "id": "s", "name": "Active", "category": "live" },
                "mode": "standard"
            }],
            "pagination_meta": { "page_size": 40 }
        })))
        .mount(&incidentio_srv)
        .await;
    Mock::given(method("GET"))
        .and(path("/v2/alerts"))
        .and(query_param("status[one_of]", "firing"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "alerts": [
                { "id": "al-1", "alert_source_id": "src-dd", "title": "HighErrorRate checkout-api",
                  "status": "firing", "attributes": [], "tags": [], "created_at": "2026-09-20T11:58:00Z" },
                { "id": "al-9", "alert_source_id": "src-dd", "title": "HighLatency payments-gateway",
                  "status": "firing", "attributes": [], "tags": [], "created_at": "2026-09-20T11:55:00Z" }
            ],
            "pagination_meta": { "page_size": 50 }
        })))
        .mount(&incidentio_srv)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/systemone"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "model": "jev-1.13.0",
            "answers": {
                "owner": { "type": "choice", "choice": "application",
                           "probabilities": { "application": 0.8, "platform": 0.2 }, "confidence": 0.75 },
                "impact": { "type": "score", "score": 2.0, "legend": { "0": "a", "1": "b", "2": "c", "3": "d" },
                            "probabilities": { "0": 0.0, "1": 0.0, "2": 1.0, "3": 0.0 }, "confidence": 0.9 },
                "actionable": { "type": "noul", "noul": 0.95 },
                "duplicate_of": { "type": "choice", "choice": "none",
                                  "probabilities": { "INC-4821": 0.1, "none": 0.9 }, "confidence": 0.8 },
                "caused_by_change": { "type": "noul", "noul": 0.2 }
            },
            "usage": { "input_tokens": 500, "output_tokens": 30 }
        })))
        .mount(&typesafe_srv)
        .await;

    // Every write endpoint is mounted and expected zero times: a call here
    // fails the test with wiremock's own message naming which write leaked
    // through, rather than a generic upstream error.
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
            .mount(&incidentio_srv)
            .await;
    }

    let typesafe = Client::builder()
        .api_key("ts")
        .base_url(typesafe_srv.uri())
        .retry(RetryPolicy::none())
        .build()
        .unwrap();
    let io = signalman::incidentio::Client::builder()
        .api_key("io")
        .base_url(incidentio_srv.uri())
        .retry(RetryPolicy::none())
        .build()
        .unwrap();
    let mut t = Triager::new(typesafe, io);
    t.write_back = write_back;
    t.changes = changes;
    (t, incidentio_srv, typesafe_srv)
}

#[tokio::test]
async fn the_five_tools_are_listed_with_read_only_annotations() {
    let _serialize = serialized().await;
    let (t, ..) = triager(WriteBack::DryRun, None).await;
    let client = connected(mcp::Server::new(t)).await;
    let tools = client.list_all_tools().await.unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t.name.as_ref()).collect();
    for name in [
        "qualify_alert",
        "related_alerts",
        "recent_changes",
        "lookup_owner",
        "open_incidents",
    ] {
        assert!(names.contains(&name), "missing {name} in {names:?}");
    }
    for tool in &tools {
        let read_only = tool
            .annotations
            .as_ref()
            .and_then(|a| a.read_only_hint)
            .unwrap_or(false);
        assert!(read_only, "{} is not marked read-only", tool.name);
    }
}

#[tokio::test]
async fn qualify_alert_by_id_returns_a_schema_valid_outcome_and_writes_nothing() {
    let _serialize = serialized().await;
    // Configured to apply writes; `mcp::Server::new` must force dry run
    // regardless, which the zero-expectation write mocks above confirm.
    let (t, ..) = triager(WriteBack::Apply, None).await;
    let client = connected(mcp::Server::new(t)).await;

    let result = client
        .call_tool(call("qualify_alert", json!({ "alert_id": "al-1" })))
        .await
        .unwrap();
    assert_ne!(result.is_error, Some(true), "{result:?}");
    let outcome: Outcome = result.clone().into_typed().unwrap();
    assert_eq!(outcome.alert.id.as_deref(), Some("al-1"));
    assert_eq!(format!("{:?}", outcome.writes.mode), "DryRun");
    assert!(!outcome.writes.tags_applied);

    let validator = jsonschema::validator_for(&committed_schema()).unwrap();
    let structured = result.structured_content.expect("structured content");
    assert!(
        validator.is_valid(&structured),
        "{:#?}",
        validator.iter_errors(&structured).collect::<Vec<_>>()
    );
    assert!(
        !result.content.is_empty(),
        "a text fallback summary must accompany the structured content"
    );
}

#[tokio::test]
async fn qualify_alert_inline_triages_a_standalone_alert_detached() {
    let _serialize = serialized().await;
    let (t, ..) = triager(WriteBack::DryRun, None).await;
    let client = connected(mcp::Server::new(t)).await;

    let alert = json!({
        "source": "prometheus",
        "title": "KubePodCrashLooping",
        "description": "checkout-api restarting",
        "labels": { "service": "checkout-api" }
    });
    let result = client
        .call_tool(call("qualify_alert", json!({ "alert": alert })))
        .await
        .unwrap();
    assert_ne!(result.is_error, Some(true), "{result:?}");
    let outcome: Outcome = result.into_typed().unwrap();
    assert_eq!(outcome.alert.id, None);
    assert_eq!(format!("{:?}", outcome.writes.mode), "Detached");
}

#[tokio::test]
async fn qualify_alert_rejects_both_or_neither_input() {
    let _serialize = serialized().await;
    let (t, ..) = triager(WriteBack::DryRun, None).await;
    let client = connected(mcp::Server::new(t)).await;

    let neither = client.call_tool(call("qualify_alert", json!({}))).await;
    assert_invalid_params(neither.expect_err("neither alert_id nor alert should be rejected"));

    let (t2, ..) = triager(WriteBack::DryRun, None).await;
    let client2 = connected(mcp::Server::new(t2)).await;
    let both = client2
        .call_tool(call(
            "qualify_alert",
            json!({ "alert_id": "al-1", "alert": {} }),
        ))
        .await;
    assert_invalid_params(both.expect_err("both should be rejected"));
}

#[tokio::test]
async fn related_alerts_lists_everything_else_firing_when_given_an_id() {
    let _serialize = serialized().await;
    let (t, ..) = triager(WriteBack::DryRun, None).await;
    let client = connected(mcp::Server::new(t)).await;

    let result = client
        .call_tool(call(
            "related_alerts",
            json!({ "alert_id": "al-1", "window_minutes": 30 }),
        ))
        .await
        .unwrap();
    assert_ne!(result.is_error, Some(true), "{result:?}");
    let rows = result.structured_content.expect("structured content");
    let titles: Vec<&str> = rows
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["title"].as_str().unwrap())
        .collect();
    assert_eq!(titles, vec!["HighLatency payments-gateway"]);
}

#[tokio::test]
async fn recent_changes_explains_itself_when_the_feed_is_not_configured() {
    let _serialize = serialized().await;
    let (t, ..) = triager(WriteBack::DryRun, None).await;
    let client = connected(mcp::Server::new(t)).await;

    let result = client
        .call_tool(call("recent_changes", json!({})))
        .await
        .unwrap();
    let body = result.structured_content.expect("structured content");
    assert_eq!(body["configured"], json!(false));
    assert_eq!(body["changes"], json!([]));
    assert!(
        body["note"]
            .as_str()
            .unwrap()
            .contains("SIGNALMAN_CHANGES_TOKEN"),
        "{body}"
    );
}

#[tokio::test]
async fn recent_changes_matches_a_configured_feed() {
    let _serialize = serialized().await;
    let log = ChangeLog::default();
    log.record(
        Change {
            at: None,
            kind: "deploy".into(),
            component: Some("checkout-api".into()),
            summary: "checkout-api v2.31.0 synced by Argo CD".into(),
            source: Some("argocd".into()),
            url: Some("https://argocd.example.com/applications/checkout-api".into()),
        },
        Timestamp::now(),
    );
    let (t, ..) = triager(WriteBack::DryRun, Some(log)).await;
    let client = connected(mcp::Server::new(t)).await;

    let result = client
        .call_tool(call(
            "recent_changes",
            json!({ "component": "checkout-api", "window_minutes": 60 }),
        ))
        .await
        .unwrap();
    let body = result.structured_content.expect("structured content");
    assert_eq!(body["configured"], json!(true));
    let changes = body["changes"].as_array().unwrap();
    assert_eq!(changes.len(), 1);
    assert_eq!(
        changes[0]["summary"],
        json!("checkout-api v2.31.0 synced by Argo CD")
    );
}

#[tokio::test]
async fn lookup_owner_needs_backstage_configured() {
    let _serialize = serialized().await;
    let (t, ..) = triager(WriteBack::DryRun, None).await;
    let client = connected(mcp::Server::new(t)).await;

    let err = client
        .call_tool(call("lookup_owner", json!({ "component": "checkout-api" })))
        .await
        .expect_err("Backstage is not configured on this server");
    assert_invalid_params(err);
}

#[tokio::test]
async fn lookup_owner_resolves_against_the_catalog() {
    let _serialize = serialized().await;
    let (mut t, ..) = triager(WriteBack::DryRun, None).await;
    let backstage_srv = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(
            "/api/catalog/entities/by-name/component/default/checkout-api",
        ))
        .respond_with(ResponseTemplate::new(404))
        .mount(&backstage_srv)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/catalog/entities/by-query"))
        .and(query_param(
            "filter",
            "kind=component,metadata.title=checkout-api",
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({ "items": [], "totalItems": 0, "pageInfo": {} })),
        )
        .mount(&backstage_srv)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/catalog/entities/by-query"))
        .and(query_param("filter", "kind=group,spec.type=team"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [{
                "apiVersion": "backstage.io/v1alpha1", "kind": "Group",
                "metadata": { "name": "payments", "namespace": "default", "description": "Owns checkout" },
                "spec": { "type": "team" },
                "relations": [{ "type": "hasMember", "targetRef": "user:default/ada" }]
            }],
            "totalItems": 1, "pageInfo": {}
        })))
        .mount(&backstage_srv)
        .await;
    let backstage_client = signalman::backstage::Client::builder()
        .base_url(backstage_srv.uri())
        .build()
        .unwrap();
    t.backstage = Some(Enricher::new(backstage_client));

    let client = connected(mcp::Server::new(t)).await;
    let result = client
        .call_tool(call(
            "lookup_owner",
            json!({ "component": "checkout-api", "text": "5xx errors" }),
        ))
        .await
        .unwrap();
    assert_ne!(result.is_error, Some(true), "{result:?}");
    let body = result.structured_content.expect("structured content");
    assert!(body["component"].is_null());
    let candidates = body["owner_candidates"].as_array().unwrap();
    assert!(
        candidates.iter().any(|c| c["key"] == json!("payments")),
        "{candidates:?}"
    );
}

#[tokio::test]
async fn open_incidents_lists_what_incident_io_has_open() {
    let _serialize = serialized().await;
    let (t, ..) = triager(WriteBack::DryRun, None).await;
    let client = connected(mcp::Server::new(t)).await;

    let result = client
        .call_tool(call("open_incidents", json!({})))
        .await
        .unwrap();
    let body = result.structured_content.expect("structured content");
    let rows = body.as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["reference"], json!("INC-4821"));
    assert_eq!(rows[0]["id"], json!("01INC4821"));
}
