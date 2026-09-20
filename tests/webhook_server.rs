//! End to end: a signed incident.io webhook → alert and live incidents fetched
//! from a mock incident.io → mock TypeSafe answers → tags added and the alert
//! attached to the duplicate incident.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::too_many_lines)]

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::json;
use signalman::backstage::{self, Enricher};
use signalman::incidentio::webhook::WebhookSecret;
use signalman::incidentio::{self, Triager, WriteBack};
use signalman::serve::{AppState, router};
use signalman::triage::Decision;
use signalman::{Client, RetryPolicy};
use tower::ServiceExt;
use wiremock::matchers::{body_json, body_partial_json, body_string_contains, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const SECRET: &str = "whsec_plJ3nmyCDGBKInavdOK15jsl";

fn now_secs() -> i64 {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs(),
    )
    .unwrap()
}

fn signed(body: &str, id: &str, secret: &str) -> Request<Body> {
    let ts = now_secs();
    let sig = WebhookSecret::parse(secret)
        .unwrap()
        .sign(id, ts, body.as_bytes());
    Request::builder()
        .method("POST")
        .uri("/webhooks/incidentio")
        .header("content-type", "application/json")
        .header("webhook-id", id)
        .header("webhook-timestamp", ts.to_string())
        .header("webhook-signature", sig)
        .body(Body::from(body.to_owned()))
        .unwrap()
}

fn alert_created(alert_id: &str) -> String {
    json!({
        "event_type": "public_alert.alert_created_v1",
        "public_alert.alert_created_v1": {
            "id": alert_id, "alert_source_id": "src-dd", "title": "HighErrorRate checkout-api",
            "status": "firing", "deduplication_key": "dd:123", "attributes": [], "tags": []
        }
    })
    .to_string()
}

struct Harness {
    app: axum::Router,
    outcomes: tokio::sync::mpsc::UnboundedReceiver<Result<incidentio::Outcome, String>>,
    incidentio: MockServer,
    _typesafe: MockServer,
}

async fn harness(write_back: WriteBack, expect_triage: bool) -> Harness {
    let incidentio_srv = MockServer::start().await;
    let typesafe_srv = MockServer::start().await;

    // Latest alert state: richer than the webhook payload (attributes appear).
    Mock::given(method("GET"))
        .and(path("/v2/alerts/al-1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "alert": {
                "id": "al-1", "alert_source_id": "src-dd", "title": "HighErrorRate checkout-api",
                "description": "5xx ratio 12% for 10m on checkout-api", "status": "firing",
                "deduplication_key": "dd:123",
                "attributes": [{
                    "attribute": { "id": "x", "name": "Service", "array": false, "required": false, "type": "String" },
                    "value": { "literal": "checkout-api", "label": "checkout-api" }
                }],
                "tags": []
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
                "incident_status": { "id": "s", "name": "Active", "category": "live" },
                "severity": { "id": "sev", "name": "Major", "rank": 2 }, "mode": "standard"
            }],
            "pagination_meta": { "page_size": 40 }
        })))
        .mount(&incidentio_srv)
        .await;

    // TypeSafe must have been offered INC-4821 and `none` as dedup options.
    Mock::given(method("POST"))
        .and(path("/v1/systemone"))
        .and(body_partial_json(json!({
            "state": { "alert": { "labels": { "Service": "checkout-api" }, "open_incidents": [{ "id": "INC-4821" }] } },
            "questions": { "duplicate_of": { "criteria": { "INC-4821": "Checkout 5xx spike [Major]: payments-gateway returning errors" } } }
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "model": "jev-1.13.0",
            "answers": {
                "owner": { "type": "choice", "choice": "application",
                           "probabilities": { "application": 0.8, "platform": 0.2 }, "confidence": 0.7 },
                "impact": { "type": "score", "score": 2.0, "legend": { "0": "a", "1": "b", "2": "c", "3": "d" },
                            "probabilities": { "0": 0.0, "1": 0.0, "2": 1.0, "3": 0.0 }, "confidence": 1.0 },
                "actionable": { "type": "noul", "noul": 0.95 },
                "duplicate_of": { "type": "choice", "choice": "INC-4821",
                                  "probabilities": { "INC-4821": 0.93, "none": 0.07 }, "confidence": 0.86 }
            },
            "usage": { "input_tokens": 700, "output_tokens": 40 }
        })))
        .mount(&typesafe_srv)
        .await;

    // Write-back expectations.
    Mock::given(method("POST"))
        .and(path("/v2/alerts/al-1/actions/add_tags"))
        .and(body_json(json!({ "tags": ["ai-team-application", "ai-impact-major", "ai-action-attach", "ai-dup-inc-4821"] })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "alert": { "id": "al-1", "alert_source_id": "src-dd", "title": "t", "status": "firing", "attributes": [], "tags": [] }
        })))
        .expect(u64::from(expect_triage && write_back == WriteBack::Apply))
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
        .expect(u64::from(expect_triage && write_back == WriteBack::Apply))
        .mount(&incidentio_srv)
        .await;

    let typesafe = Client::builder()
        .api_key("ts")
        .base_url(typesafe_srv.uri())
        .retry(RetryPolicy::none())
        .build()
        .unwrap();
    let io = incidentio::Client::builder()
        .api_key("io")
        .base_url(incidentio_srv.uri())
        .retry(RetryPolicy::none())
        .build()
        .unwrap();
    let mut triager = Triager::new(typesafe, io);
    triager.write_back = write_back;

    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let mut state = AppState::new(Some(WebhookSecret::parse(SECRET).unwrap()), triager);
    state.on_outcome = Some(tx);
    Harness {
        app: router(Arc::new(state)),
        outcomes: rx,
        incidentio: incidentio_srv,
        _typesafe: typesafe_srv,
    }
}

#[tokio::test]
async fn signed_alert_created_webhook_triages_tags_and_attaches() {
    let mut h = harness(WriteBack::Apply, true).await;
    let resp = h
        .app
        .clone()
        .oneshot(signed(&alert_created("al-1"), "msg-1", SECRET))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::ACCEPTED);

    let outcome = tokio::time::timeout(Duration::from_secs(5), h.outcomes.recv())
        .await
        .expect("outcome in time")
        .expect("channel open")
        .expect("triage succeeded");
    assert_eq!(outcome.alert_id, "al-1");
    assert!(
        matches!(outcome.decision, Decision::AttachToIncident { ref incident_id, .. } if incident_id == "INC-4821")
    );
    assert_eq!(outcome.attached_to.as_ref().unwrap().id, "01INC4821");
    assert_eq!(outcome.candidates_offered, 1);
    assert!(outcome.applied);
    h.incidentio.verify().await;
}

#[tokio::test]
async fn dry_run_decides_but_writes_nothing() {
    let mut h = harness(WriteBack::DryRun, true).await;
    let resp = h
        .app
        .clone()
        .oneshot(signed(&alert_created("al-1"), "msg-2", SECRET))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::ACCEPTED);
    let outcome = tokio::time::timeout(Duration::from_secs(5), h.outcomes.recv())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(!outcome.applied);
    assert_eq!(
        outcome.tags,
        vec![
            "ai-team-application",
            "ai-impact-major",
            "ai-action-attach",
            "ai-dup-inc-4821"
        ]
    );
    h.incidentio.verify().await;
}

#[tokio::test]
async fn bad_signature_is_401_and_nothing_runs() {
    let h = harness(WriteBack::Apply, false).await;
    let resp = h
        .app
        .clone()
        .oneshot(signed(
            &alert_created("al-1"),
            "msg-3",
            "whsec_MfKQ9r8GKYqrTwjUPD8ILPZIo2LaLaSw",
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    let text = String::from_utf8(
        resp.into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec(),
    )
    .unwrap();
    assert!(text.contains("signature"), "{text}");
    assert!(h.incidentio.received_requests().await.unwrap().is_empty());

    let unsigned = Request::builder()
        .method("POST")
        .uri("/webhooks/incidentio")
        .body(Body::from(alert_created("al-1")))
        .unwrap();
    assert_eq!(
        h.app.clone().oneshot(unsigned).await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn duplicate_delivery_and_other_events_are_acknowledged_without_work() {
    let mut h = harness(WriteBack::Apply, true).await;
    let first = h
        .app
        .clone()
        .oneshot(signed(&alert_created("al-1"), "msg-dup", SECRET))
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::ACCEPTED);
    let _ = tokio::time::timeout(Duration::from_secs(5), h.outcomes.recv())
        .await
        .unwrap();

    let again = h
        .app
        .clone()
        .oneshot(signed(&alert_created("al-1"), "msg-dup", SECRET))
        .await
        .unwrap();
    assert_eq!(again.status(), StatusCode::OK);

    let other = json!({ "event_type": "schedule.shift_change_v1", "schedule.shift_change_v1": {} })
        .to_string();
    assert_eq!(
        h.app
            .clone()
            .oneshot(signed(&other, "msg-other", SECRET))
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );

    let junk = signed("not json", "msg-junk", SECRET);
    assert_eq!(
        h.app.clone().oneshot(junk).await.unwrap().status(),
        StatusCode::BAD_REQUEST
    );

    // Exactly one triage ran.
    let add_tags = h
        .incidentio
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path().ends_with("/actions/add_tags"))
        .count();
    assert_eq!(add_tags, 1);
}

/// With Backstage configured: the alert's Service attribute resolves a
/// component, its owner group and neighbours become the owner options, the
/// TechDocs runbook lands in the state, the tag names the group, and the
/// group is notified.
#[tokio::test]
async fn backstage_enrichment_drives_owner_candidates_runbook_and_notification() {
    let incidentio_srv = MockServer::start().await;
    let typesafe_srv = MockServer::start().await;
    let backstage_srv = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/v2/alerts/al-2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "alert": {
                "id": "al-2", "alert_source_id": "src-dd", "title": "HighErrorRate checkout-api",
                "description": "5xx ratio 12% for 10m", "status": "firing", "deduplication_key": "dd:2",
                "source_url": "https://app.datadoghq.eu/monitors/2",
                "attributes": [{
                    "attribute": { "id": "x", "name": "Service", "array": false, "required": false, "type": "String" },
                    "value": { "literal": "checkout-api", "label": "checkout-api" }
                }],
                "tags": []
            }
        })))
        .mount(&incidentio_srv)
        .await;
    Mock::given(method("GET"))
        .and(path("/v2/incidents"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({ "incidents": [], "pagination_meta": { "page_size": 40 } })),
        )
        .mount(&incidentio_srv)
        .await;
    Mock::given(method("POST"))
        .and(path("/v2/alerts/al-2/actions/add_tags"))
        .and(body_json(json!({ "tags": ["ai-team-payments", "ai-impact-major", "ai-action-page"] })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "alert": { "id": "al-2", "alert_source_id": "src-dd", "title": "t", "status": "firing", "attributes": [], "tags": [] }
        })))
        .expect(1)
        .mount(&incidentio_srv)
        .await;

    // Catalog.
    Mock::given(method("GET"))
        .and(path("/api/catalog/entities/by-name/component/default/checkout-api"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "kind": "Component",
            "metadata": { "name": "checkout-api", "namespace": "default", "title": "Checkout API",
                          "annotations": { "backstage.io/techdocs-ref": "dir:." } },
            "spec": { "type": "service", "lifecycle": "production", "owner": "group:default/payments" },
            "relations": [
                { "type": "ownedBy", "targetRef": "group:default/payments" },
                { "type": "dependsOn", "targetRef": "resource:default/orders-db" }
            ]
        })))
        .mount(&backstage_srv)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/catalog/entities/by-refs"))
        .and(body_string_contains("resource:default/orders-db"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "items": [
            { "kind": "Resource", "metadata": { "name": "orders-db", "namespace": "default" }, "spec": {},
              "relations": [{ "type": "ownedBy", "targetRef": "group:default/data-platform" }] }
        ] })))
        .with_priority(1)
        .mount(&backstage_srv)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/catalog/entities/by-refs"))
        .and(body_string_contains("group:default/payments"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "items": [
            { "kind": "Group", "metadata": { "name": "payments", "namespace": "default", "description": "Checkout and payments" },
              "spec": { "type": "team", "profile": { "displayName": "Payments" } }, "relations": [] },
            { "kind": "Group", "metadata": { "name": "data-platform", "namespace": "default" },
              "spec": { "type": "team", "profile": { "displayName": "Data Platform" } }, "relations": [] }
        ] })))
        .with_priority(1)
        .mount(&backstage_srv)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/catalog/entities/by-refs"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "items": [] })))
        .with_priority(9)
        .mount(&backstage_srv)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/techdocs/static/docs/default/component/checkout-api/search/search_index.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "docs": [
            { "location": "runbooks/high-error-rate/", "title": "High error rate", "text": "Roll back the last deploy." }
        ] })))
        .mount(&backstage_srv)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/notifications/notifications"))
        .and(body_partial_json(json!({
            "recipients": { "type": "entity", "entityRef": "group:default/payments" },
            "payload": { "title": "Page: HighErrorRate checkout-api", "severity": "high", "topic": "signalman", "link": "https://app.datadoghq.eu/monitors/2" }
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .expect(1)
        .mount(&backstage_srv)
        .await;

    // TypeSafe must see the catalog candidates, the component context and the runbook.
    Mock::given(method("POST"))
        .and(path("/v1/systemone"))
        .and(body_partial_json(json!({
            "state": { "alert": {
                "component": { "name": "checkout-api", "owner": "Payments", "depends_on": ["resource orders-db"] },
                "runbook": "High error rate (runbooks/high-error-rate): Roll back the last deploy."
            } },
            "questions": { "owner": { "criteria": { "payments": "Payments: Checkout and payments", "data-platform": "Data Platform", "none_of_these": "Not clearly attributable to any listed team from the information given" } } }
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "model": "jev-1.13.0",
            "answers": {
                "owner": { "type": "choice", "choice": "payments",
                           "probabilities": { "payments": 0.9, "data-platform": 0.08, "none_of_these": 0.02 }, "confidence": 0.85 },
                "impact": { "type": "score", "score": 2.0, "legend": { "0": "a", "1": "b", "2": "c", "3": "d" },
                            "probabilities": { "0": 0.0, "1": 0.0, "2": 1.0, "3": 0.0 }, "confidence": 1.0 },
                "actionable": { "type": "noul", "noul": 0.95 }
            },
            "usage": { "input_tokens": 900, "output_tokens": 40 }
        })))
        .expect(1)
        .mount(&typesafe_srv)
        .await;

    let typesafe = Client::builder()
        .api_key("ts")
        .base_url(typesafe_srv.uri())
        .retry(RetryPolicy::none())
        .build()
        .unwrap();
    let io = incidentio::Client::builder()
        .api_key("io")
        .base_url(incidentio_srv.uri())
        .retry(RetryPolicy::none())
        .build()
        .unwrap();
    let bs = backstage::Client::builder()
        .base_url(backstage_srv.uri())
        .token("bs")
        .retry(RetryPolicy::none())
        .build()
        .unwrap();
    let mut triager = Triager::new(typesafe, io);
    triager.backstage = Some(Enricher::new(bs));
    triager.notify_owner = true;

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let mut state = AppState::new(Some(WebhookSecret::parse(SECRET).unwrap()), triager);
    state.on_outcome = Some(tx);
    let app = router(Arc::new(state));

    let body = json!({
        "event_type": "public_alert.alert_created_v1",
        "public_alert.alert_created_v1": { "id": "al-2", "alert_source_id": "src-dd", "title": "HighErrorRate checkout-api", "status": "firing", "attributes": [], "tags": [] }
    }).to_string();
    let resp = app
        .clone()
        .oneshot(signed(&body, "msg-bs", SECRET))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::ACCEPTED);

    let outcome = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(outcome.component.as_deref(), Some("checkout-api"));
    assert_eq!(outcome.notified.as_deref(), Some("group:default/payments"));
    assert_eq!(outcome.owner_candidates_offered, 3);
    match outcome.decision {
        Decision::Page { owner, .. } => {
            assert_eq!(owner.key, "payments");
            assert_eq!(owner.entity_ref.as_deref(), Some("group:default/payments"));
        }
        other => panic!("{other:?}"),
    }
    incidentio_srv.verify().await;
    backstage_srv.verify().await;
    typesafe_srv.verify().await;
}
