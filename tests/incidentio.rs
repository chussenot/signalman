//! incident.io client against a mock server: query shape, pagination,
//! auth per endpoint, error mapping, retry on 429.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Duration;

use serde_json::json;
use signalman::RetryPolicy;
use signalman::incidentio::types::{AlertEvent, AlertStatus};
use signalman::incidentio::{Client, Error};
use wiremock::matchers::{body_json, header, method, path, query_param};

use wiremock::{Mock, MockServer, ResponseTemplate};

fn client(server: &MockServer) -> Client {
    Client::builder()
        .api_key("inc-key")
        .base_url(server.uri())
        .retry(RetryPolicy {
            max_retries: 2,
            backoff_initial: Duration::from_millis(5),
            backoff_max: Duration::from_millis(20),
            backoff_jitter: 0.0,
            retry_after_max: Duration::from_secs(1),
        })
        .build()
        .unwrap()
}

fn incident(id: &str, reference: &str, category: &str, mode: &str) -> serde_json::Value {
    json!({
        "id": id, "reference": reference, "name": format!("Incident {reference}"),
        "incident_status": { "id": "s", "name": category, "category": category },
        "mode": mode, "created_at": "2026-09-20T10:00:00Z", "updated_at": "2026-09-20T10:00:00Z",
        "custom_field_entries": [], "team_ids": [], "creator": {}, "slack_channel_id": "C1", "slack_team_id": "T1",
        "incident_role_assignments": [], "last_activity_at": "2026-09-20T10:00:00Z"
    })
}

#[tokio::test]
async fn lists_open_incidents_with_category_filters_and_pagination() {
    let server = MockServer::start().await;
    // Page 1: full page of 2, includes a test incident to be filtered out.
    Mock::given(method("GET"))
        .and(path("/v2/incidents"))
        .and(header("authorization", "Bearer inc-key"))
        .and(query_param("status_category[one_of]", "triage"))
        .and(query_param("page_size", "2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "incidents": [incident("i1", "INC-1", "live", "standard"), incident("i2", "INC-2", "live", "test")],
            "pagination_meta": { "after": "i2", "page_size": 2 }
        })))
        .expect(1)
        .mount(&server)
        .await;
    // Page 2: short page ends iteration.
    Mock::given(method("GET"))
        .and(path("/v2/incidents"))
        .and(query_param("after", "i2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "incidents": [incident("i3", "INC-3", "triage", "standard")],
            "pagination_meta": { "page_size": 1 }
        })))
        .expect(1)
        .mount(&server)
        .await;

    let got = client(&server).list_open_incidents(2).await.unwrap();
    let refs: Vec<&str> = got.iter().map(|i| i.reference.as_str()).collect();
    assert_eq!(refs, vec!["INC-1", "INC-3"]);

    // The live filter was sent as a repeated key alongside triage.
    let requests = server.received_requests().await.unwrap();
    let q = requests[0].url.query().unwrap();
    assert!(q.contains("status_category%5Bone_of%5D=live"), "{q}");
    assert!(q.contains("status_category%5Bone_of%5D=paused"), "{q}");
}

#[tokio::test]
async fn add_tags_and_attach_use_documented_bodies() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v2/alerts/a1/actions/add_tags"))
        .and(header("authorization", "Bearer inc-key"))
        .and(body_json(json!({ "tags": ["ai-team-database", "ai-action-page"] })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "alert": { "id": "a1", "alert_source_id": "s", "title": "t", "status": "firing",
                       "deduplication_key": "k", "attributes": [], "tags": [{"id": "t1", "name": "ai-team-database"}] }
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v2/incident_alerts"))
        .and(body_json(json!({ "alert_id": "a1", "incident_id": "i9" })))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "incident_alert": { "id": "ia1", "alert": {}, "incident": {} }
        })))
        .expect(1)
        .mount(&server)
        .await;

    let c = client(&server);
    let alert = c
        .add_alert_tags("a1", &["ai-team-database".into(), "ai-action-page".into()])
        .await
        .unwrap();
    assert_eq!(alert.tag_names(), vec!["ai-team-database"]);
    let link = c.attach_alert_to_incident("a1", "i9").await.unwrap();
    assert_eq!(link.id, "ia1");
}

#[tokio::test]
async fn alert_events_authenticate_with_the_source_token_not_the_api_key() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v2/alert_events/http/cfg-1"))
        .and(header("authorization", "Bearer source-token"))
        .and(body_json(json!({
            "title": "DNS down", "status": "firing", "deduplication_key": "dns:1",
            "metadata": { "ai": { "team": "network" } }
        })))
        .respond_with(ResponseTemplate::new(202).set_body_json(json!({
            "deduplication_key": "dns:1", "message": "Event accepted", "status": "success"
        })))
        .expect(1)
        .mount(&server)
        .await;

    let ack = client(&server)
        .send_alert_event(
            "cfg-1",
            "source-token",
            &AlertEvent {
                title: "DNS down".into(),
                description: None,
                status: AlertStatus::Firing,
                deduplication_key: Some("dns:1".into()),
                source_url: None,
                metadata: Some(json!({ "ai": { "team": "network" } })),
            },
        )
        .await
        .unwrap();
    assert_eq!(ack.status, "success");
}

#[tokio::test]
async fn maps_422_body_and_retries_429_with_retry_after() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v2/incident_alerts"))
        .respond_with(ResponseTemplate::new(422).set_body_json(json!({
            "type": "validation_error", "status": 422, "request_id": "req-9",
            "errors": [{ "code": "unrelated", "message": "marked unrelated", "source": { "field": "alert_id" } }]
        })))
        .expect(1)
        .mount(&server)
        .await;
    let c = client(&server);
    match c.attach_alert_to_incident("a", "i").await {
        Err(Error::Validation { detail, request_id }) => {
            assert!(detail.contains("unrelated (alert_id)"), "{detail}");
            assert_eq!(request_id, "req-9");
        }
        other => panic!("{other:?}"),
    }

    server.reset().await;
    Mock::given(method("GET"))
        .and(path("/v2/alerts/a1"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("retry-after", "0")
                .set_body_json(json!({
                    "type": "too_many_requests", "status": 429, "errors": []
                })),
        )
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v2/alerts/a1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "alert": { "id": "a1", "alert_source_id": "s", "title": "t", "status": "firing", "attributes": [], "tags": [] }
        })))
        .expect(1)
        .mount(&server)
        .await;
    assert_eq!(c.get_alert("a1").await.unwrap().id, "a1");
}

#[tokio::test]
async fn unauthorized_is_not_retried() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/identity"))
        .respond_with(
            ResponseTemplate::new(401)
                .set_body_json(json!({ "type": "unauthorized", "request_id": "r" })),
        )
        .expect(1)
        .mount(&server)
        .await;
    assert!(matches!(
        client(&server).identity().await,
        Err(Error::Unauthorized { .. })
    ));
}

#[tokio::test]
async fn firing_alerts_and_alert_notes_use_documented_shapes() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v2/alerts"))
        .and(header("authorization", "Bearer inc-key"))
        .and(query_param("status[one_of]", "firing"))
        .and(query_param("created_at[gte]", "2026-09-20T11:30:00Z"))
        .and(query_param("page_size", "50"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "alerts": [{ "id": "a2", "alert_source_id": "s", "title": "Other", "status": "firing",
                         "attributes": [], "tags": [], "created_at": "2026-09-20T11:45:00Z" }],
            "pagination_meta": { "page_size": 50 }
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/alert_notes"))
        .and(query_param("alert_id", "a1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "alert_notes": [
                { "id": "n1", "alert_id": "a1", "content": "**Signalman qualification**\n\nold",
                  "created_at": "2026-09-20T11:00:00Z", "creator": { "api_key": { "id": "k", "name": "signalman" } } },
                { "id": "n2", "alert_group_id": "g1", "content": "group note" }
            ],
            "pagination_meta": { "page_size": 50 }
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/alert_notes"))
        .and(body_json(json!({ "alert_id": "a1", "content": "# new" })))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "alert_note": { "id": "n3", "alert_id": "a1", "content": "# new" }
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/v1/alert_notes/n1"))
        .and(body_json(json!({ "content": "# replaced" })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "alert_note": { "id": "n1", "alert_id": "a1", "content": "# replaced" }
        })))
        .expect(1)
        .mount(&server)
        .await;

    let c = client(&server);
    let firing = c
        .list_firing_alerts_since("2026-09-20T11:30:00Z", 50)
        .await
        .unwrap();
    assert_eq!(firing.len(), 1);
    assert_eq!(
        firing[0].created_at.as_deref(),
        Some("2026-09-20T11:45:00Z")
    );

    let notes = c.list_alert_notes("a1").await.unwrap();
    assert_eq!(notes.len(), 2);
    assert_eq!(notes[1].alert_id, None);

    let created = c.create_alert_note("a1", "# new").await.unwrap();
    assert_eq!(created.id, "n3");
    let replaced = c.update_alert_note("n1", "# replaced").await.unwrap();
    assert_eq!(replaced.content, "# replaced");
}
