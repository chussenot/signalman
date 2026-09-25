//! The MCP server end to end: a real client, over an in-process transport,
//! driving each tool against mock TypeSafe, incident.io and Backstage
//! servers. `qualify_alert` is checked against the committed outcome schema
//! and, separately, against a `Triager` explicitly configured to apply
//! writes — proving `mcp::Server::new` forces dry run regardless.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::too_many_lines)]

mod common;

use common::{DummyClient, call, committed_schema};
use jiff::Timestamp;
use rmcp::ServiceExt;
use rmcp::model::ErrorCode;
use rmcp::service::ServiceError;
use serde_json::json;
use signalman::backstage::Enricher;
use signalman::changes::{Change, ChangeLog};
use signalman::incidentio::{Triager, WriteBack};
use signalman::mcp;
use signalman::outcome::Outcome;
use wiremock::matchers::{
    body_json, body_partial_json, body_string_contains, method, path, query_param,
};
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
    common::mount_scene(&incidentio_srv).await;
    common::SystemOne::default().mount(&typesafe_srv).await;
    common::mount_no_writes(&incidentio_srv).await;

    let mut t = common::triager(&typesafe_srv, &incidentio_srv);
    t.write_back = write_back;
    t.changes = changes;
    (t, incidentio_srv, typesafe_srv)
}

#[tokio::test]
async fn the_five_tools_are_listed_with_read_only_annotations() {
    let _serialize = serialized().await;
    let (t, ..) = triager(WriteBack::DryRun, None).await;
    let client = connected(mcp::Server::new(t, false)).await;
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
    let client = connected(mcp::Server::new(t, false)).await;

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
    let client = connected(mcp::Server::new(t, false)).await;

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
    let client = connected(mcp::Server::new(t, false)).await;

    let neither = client.call_tool(call("qualify_alert", json!({}))).await;
    assert_invalid_params(neither.expect_err("neither alert_id nor alert should be rejected"));

    let (t2, ..) = triager(WriteBack::DryRun, None).await;
    let client2 = connected(mcp::Server::new(t2, false)).await;
    let both = client2
        .call_tool(call(
            "qualify_alert",
            json!({ "alert_id": "al-1", "alert": {} }),
        ))
        .await;
    assert_invalid_params(both.expect_err("both should be rejected"));
}

#[tokio::test]
async fn qualify_alert_reports_an_unfit_answer_as_a_tool_error_on_both_inputs() {
    // The wording docs/mcp.md gives an agent: the client's message, after
    // `qualify_alert: ` for an id and `qualify_alert: TypeSafe call failed: `
    // for an inline alert, naming the question and the option, ending with
    // the request id the API sent. Nothing is written either way.
    let _serialize = serialized().await;
    let incidentio_srv = MockServer::start().await;
    let typesafe_srv = MockServer::start().await;
    common::mount_scene(&incidentio_srv).await;
    common::mount_no_writes(&incidentio_srv).await;
    let mut body = common::SystemOne::default().body();
    body["answers"]["owner"] = json!({ "type": "choice", "choice": "made-up-team",
                                       "probabilities": { "made-up-team": 0.9, "application": 0.1 },
                                       "confidence": 0.9 });
    Mock::given(method("POST"))
        .and(path("/v1/systemone"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-typesafe-request-id", "req-unfit")
                .set_body_json(body),
        )
        .expect(2)
        .mount(&typesafe_srv)
        .await;
    let t = common::triager(&typesafe_srv, &incidentio_srv);
    let client = connected(mcp::Server::new(t, false)).await;

    let inline = json!({ "source": "prometheus", "title": "KubePodCrashLooping",
                         "description": "checkout-api restarting" });
    for (args, prefix) in [
        (
            json!({ "alert_id": "al-1" }),
            "qualify_alert: answer \"owner\"",
        ),
        (
            json!({ "alert": inline }),
            "qualify_alert: TypeSafe call failed: answer \"owner\"",
        ),
    ] {
        let result = client.call_tool(call("qualify_alert", args)).await.unwrap();
        assert_eq!(result.is_error, Some(true), "{result:?}");
        let text = &result.content[0].as_text().expect("a text error").text;
        assert!(text.starts_with(prefix), "{text}");
        assert!(text.contains("made-up-team"), "{text}");
        assert!(text.ends_with(" [request_id req-unfit]"), "{text}");
    }
    typesafe_srv.verify().await;
    incidentio_srv.verify().await;
}

#[tokio::test]
async fn related_alerts_lists_everything_else_firing_when_given_an_id() {
    let _serialize = serialized().await;
    let (t, ..) = triager(WriteBack::DryRun, None).await;
    let client = connected(mcp::Server::new(t, false)).await;

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
    let client = connected(mcp::Server::new(t, false)).await;

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
    let client = connected(mcp::Server::new(t, false)).await;

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
    let client = connected(mcp::Server::new(t, false)).await;

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

    let client = connected(mcp::Server::new(t, false)).await;
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
    let client = connected(mcp::Server::new(t, false)).await;

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

// ---------------------------------------------------------------------------
// apply_qualification
// ---------------------------------------------------------------------------

/// The same read fixtures as `triager()` (alert `al-1`, one open incident
/// `INC-4821`, one other firing alert), but with the write endpoints mocked
/// to succeed and asserted against precisely, for `apply_qualification`
/// tests. `duplicate_choice` picks the model's `duplicate_of` answer:
/// `"none"` decides `page` (tags and a note, no attachment); `"INC-4821"`
/// decides `attach_to_incident` (tags, a note, and the attachment).
/// `POST /v1/incidents` is mounted with a zero-call expectation: signalman
/// never creates one (decision 0001), whichever path is under test.
async fn write_scenario(duplicate_choice: &str) -> (Triager, MockServer, MockServer) {
    let incidentio_srv = MockServer::start().await;
    let typesafe_srv = MockServer::start().await;
    common::mount_alert_al1(&incidentio_srv).await;
    common::mount_open_incidents(&incidentio_srv, vec![common::incident_inc4821()]).await;
    common::mount_firing_alerts(&incidentio_srv, vec![common::firing_al1()]).await;
    let mut answers = common::SystemOne {
        duplicate_confidence: 0.85,
        ..common::SystemOne::default()
    };
    if duplicate_choice == common::INCIDENT_REF {
        answers = answers.attaching();
    }
    answers.mount(&typesafe_srv).await;

    let mut expected_tags = vec!["ai-team-application", "ai-impact-major"];
    if duplicate_choice == "INC-4821" {
        expected_tags.push("ai-action-attach");
        expected_tags.push("ai-dup-inc-4821");
    } else {
        expected_tags.push("ai-action-page");
    }
    Mock::given(method("POST"))
        .and(path("/v2/alerts/al-1/actions/add_tags"))
        .and(body_json(json!({ "tags": expected_tags })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "alert": { "id": "al-1", "alert_source_id": "src-dd", "title": "t", "status": "firing", "attributes": [], "tags": [] }
        })))
        .expect(1)
        .mount(&incidentio_srv)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/alert_notes"))
        .and(query_param("alert_id", "al-1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "alert_notes": [] })))
        .mount(&incidentio_srv)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/alert_notes"))
        .and(body_partial_json(json!({ "alert_id": "al-1" })))
        .and(body_string_contains("**Signalman qualification**"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "alert_note": { "id": "note-1", "alert_id": "al-1", "content": "…" }
        })))
        .expect(1)
        .mount(&incidentio_srv)
        .await;
    Mock::given(method("POST"))
        .and(path("/v2/incident_alerts"))
        .and(body_json(
            json!({ "alert_id": "al-1", "incident_id": "01INC4821" }),
        ))
        .respond_with(
            ResponseTemplate::new(201).set_body_json(json!({ "incident_alert": { "id": "ia" } })),
        )
        .expect(u64::from(duplicate_choice == "INC-4821"))
        .mount(&incidentio_srv)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/incidents"))
        .respond_with(ResponseTemplate::new(201))
        .expect(0)
        .mount(&incidentio_srv)
        .await;

    (
        common::triager(&typesafe_srv, &incidentio_srv),
        incidentio_srv,
        typesafe_srv,
    )
}

/// Round-trip through the two tools an agent actually uses: qualify, then
/// apply. `apply_qualification` re-derives everything it writes from the
/// document `qualify_alert` returned; nothing here is a free parameter.
#[tokio::test]
async fn apply_qualification_applies_tags_and_a_note_for_a_page_decision() {
    let _serialize = serialized().await;
    // Bind the mock servers, not `..`: an unbound tuple field is dropped at
    // the end of *this* statement, which would tear the servers down (and
    // fail their `expect(1)` verification) before any request is made.
    let (t, _incidentio_srv, _typesafe_srv) = write_scenario("none").await;
    let client = connected(mcp::Server::new(t, true)).await;

    let qualified = client
        .call_tool(call("qualify_alert", json!({ "alert_id": "al-1" })))
        .await
        .unwrap();
    let outcome: Outcome = qualified.into_typed().unwrap();
    assert_eq!(format!("{:?}", outcome.decision), "Page");

    let applied = client
        .call_tool(call(
            "apply_qualification",
            json!({ "alert_id": "al-1", "outcome": outcome }),
        ))
        .await
        .unwrap();
    assert_ne!(applied.is_error, Some(true), "{applied:?}");
    let writes = applied.structured_content.expect("structured content");
    assert_eq!(writes["mode"], json!("applied"));
    assert_eq!(writes["tags_applied"], json!(true));
    assert_eq!(writes["attached"], json!(false));
    assert_eq!(writes["note"]["status"], json!("created"));
    assert_eq!(writes["note"]["id"], json!("note-1"));
}

/// The attach path: the same round trip, with the model choosing the open
/// incident instead of `none`.
#[tokio::test]
async fn apply_qualification_attaches_the_incident_for_a_duplicate_decision() {
    let _serialize = serialized().await;
    let (t, _incidentio_srv, _typesafe_srv) = write_scenario("INC-4821").await;
    let client = connected(mcp::Server::new(t, true)).await;

    let qualified = client
        .call_tool(call("qualify_alert", json!({ "alert_id": "al-1" })))
        .await
        .unwrap();
    let outcome: Outcome = qualified.into_typed().unwrap();
    assert_eq!(format!("{:?}", outcome.decision), "AttachToIncident");

    let applied = client
        .call_tool(call(
            "apply_qualification",
            json!({ "alert_id": "al-1", "outcome": outcome }),
        ))
        .await
        .unwrap();
    assert_ne!(applied.is_error, Some(true), "{applied:?}");
    let writes = applied.structured_content.expect("structured content");
    assert_eq!(writes["attached"], json!(true));
    assert_eq!(writes["note"]["status"], json!("created"));
}

/// Calling `apply_qualification` twice with the same outcome is safe: tags
/// are reapplied harmlessly, and the note is rewritten in place through the
/// same `write_note` path the rest of the flow uses — never a second note
/// stacked alongside the first. The second `GET /v1/alert_notes` returns
/// the note the first call created, exactly as incident.io would.
#[tokio::test]
async fn apply_qualification_twice_replaces_the_note_in_place_not_stacks_it() {
    let _serialize = serialized().await;
    let incidentio_srv = MockServer::start().await;
    let typesafe_srv = MockServer::start().await;

    common::mount_alert_al1(&incidentio_srv).await;
    common::mount_open_incidents(&incidentio_srv, vec![]).await;
    common::mount_firing_alerts(&incidentio_srv, vec![common::firing_al1()]).await;
    common::SystemOne::default().mount(&typesafe_srv).await;

    Mock::given(method("POST"))
        .and(path("/v2/alerts/al-1/actions/add_tags"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "alert": { "id": "al-1", "alert_source_id": "src-dd", "title": "t", "status": "firing", "attributes": [], "tags": [] }
        })))
        .expect(2)
        .mount(&incidentio_srv)
        .await;

    // Nothing to find on the first pass; from the second call on, incident.io
    // would return the note signalman itself just created.
    Mock::given(method("GET"))
        .and(path("/v1/alert_notes"))
        .and(query_param("alert_id", "al-1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "alert_notes": [] })))
        .up_to_n_times(1)
        .mount(&incidentio_srv)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/alert_notes"))
        .and(query_param("alert_id", "al-1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "alert_notes": [{ "id": "note-1", "alert_id": "al-1", "content": "**Signalman qualification**\n…" }]
        })))
        .mount(&incidentio_srv)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/alert_notes"))
        .and(body_partial_json(json!({ "alert_id": "al-1" })))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "alert_note": { "id": "note-1", "alert_id": "al-1", "content": "…" }
        })))
        .expect(1)
        .mount(&incidentio_srv)
        .await;
    Mock::given(method("PUT"))
        .and(path("/v1/alert_notes/note-1"))
        .and(body_string_contains("**Signalman qualification**"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "alert_note": { "id": "note-1", "alert_id": "al-1", "content": "…" }
        })))
        .expect(1)
        .mount(&incidentio_srv)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/incidents"))
        .respond_with(ResponseTemplate::new(201))
        .expect(0)
        .mount(&incidentio_srv)
        .await;

    let typesafe = common::typesafe_client(&typesafe_srv.uri());
    let io = common::incidentio_client(&incidentio_srv.uri());
    let client = connected(mcp::Server::new(Triager::new(typesafe, io), true)).await;

    let qualified = client
        .call_tool(call("qualify_alert", json!({ "alert_id": "al-1" })))
        .await
        .unwrap();
    let outcome: Outcome = qualified.into_typed().unwrap();

    for expected_status in ["created", "replaced"] {
        let applied = client
            .call_tool(call(
                "apply_qualification",
                json!({ "alert_id": "al-1", "outcome": outcome }),
            ))
            .await
            .unwrap();
        assert_ne!(applied.is_error, Some(true), "{applied:?}");
        let writes = applied.structured_content.expect("structured content");
        assert_eq!(
            writes["note"]["status"],
            json!(expected_status),
            "{writes:?}"
        );
        assert_eq!(writes["note"]["id"], json!("note-1"));
    }
}

/// `apply_qualification` refuses a document built for a different alert:
/// the tool never guesses which alert a document belongs to.
#[tokio::test]
async fn apply_qualification_rejects_a_mismatched_alert_id() {
    let _serialize = serialized().await;
    // `triager()`, not `write_scenario()`: the mismatch is refused before
    // any write, and `triager()`'s fixture already expects zero calls on
    // every write endpoint.
    let (t, ..) = triager(WriteBack::DryRun, None).await;
    let client = connected(mcp::Server::new(t, true)).await;

    let qualified = client
        .call_tool(call("qualify_alert", json!({ "alert_id": "al-1" })))
        .await
        .unwrap();
    let outcome: Outcome = qualified.into_typed().unwrap();

    let err = client
        .call_tool(call(
            "apply_qualification",
            json!({ "alert_id": "al-9-not-the-alert", "outcome": outcome }),
        ))
        .await
        .expect_err("a mismatched alert_id must be refused");
    assert_invalid_params(err);
}

/// The write tool is absent from the tool list unless the server was
/// constructed with `allow_write: true` — `mcp.allow_write`, off by
/// default. A client cannot call what was never registered.
#[tokio::test]
async fn apply_qualification_is_absent_unless_allow_write_is_true() {
    let _serialize = serialized().await;
    let (t, ..) = triager(WriteBack::DryRun, None).await;
    let client = connected(mcp::Server::new(t, false)).await;

    let tools = client.list_all_tools().await.unwrap();
    assert!(
        !tools.iter().any(|t| t.name == "apply_qualification"),
        "apply_qualification must not be listed when allow_write is false: {tools:?}"
    );

    let err = client
        .call_tool(call(
            "apply_qualification",
            json!({ "alert_id": "al-1", "outcome": {} }),
        ))
        .await
        .expect_err("an unregistered tool must not be callable");
    // rmcp reports an unregistered tool name as `invalid_params` ("tool not
    // found"), not `method_not_found`.
    assert!(
        matches!(&err, ServiceError::McpError(e) if e.code == ErrorCode::INVALID_PARAMS
            && e.message.contains("not found")),
        "{err:?}"
    );
}

/// The mirror image: present, and its annotations say what it is.
#[tokio::test]
async fn apply_qualification_is_present_and_marked_as_a_write_when_allow_write_is_true() {
    let _serialize = serialized().await;
    // No tool call happens here, only `list_all_tools`: `triager()`'s
    // fixture (zero writes expected) is all this test needs.
    let (t, ..) = triager(WriteBack::DryRun, None).await;
    let client = connected(mcp::Server::new(t, true)).await;

    let tools = client.list_all_tools().await.unwrap();
    let tool = tools
        .iter()
        .find(|t| t.name == "apply_qualification")
        .expect("apply_qualification must be listed when allow_write is true");
    let read_only = tool
        .annotations
        .as_ref()
        .and_then(|a| a.read_only_hint)
        .unwrap_or(true);
    assert!(
        !read_only,
        "apply_qualification must not be read_only_hint: true"
    );
}
