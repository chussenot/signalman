//! Client behaviour against a mock TypeSafe API: happy path, error mapping,
//! retry with `Retry-After`, and end-to-end triage.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Duration;

use serde_json::json;
use signalman::triage::{Alert, Decision, OpenIncident, Policy, TriageQuestions, decide};
use signalman::{Client, Error, Questions, RetryPolicy, options};
use wiremock::matchers::{body_partial_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

options! {
    enum Department {
        Billing = "billing" => "Money",
        Technical = "technical" => "Bugs",
    }
}

fn client(server: &MockServer, retry: RetryPolicy) -> Client {
    Client::builder()
        .api_key("test-key")
        .base_url(server.uri())
        .retry(retry)
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap()
}

fn fast_retries(max: u32) -> RetryPolicy {
    RetryPolicy {
        max_retries: max,
        backoff_initial: Duration::from_millis(5),
        backoff_max: Duration::from_millis(20),
        backoff_jitter: 0.0,
        retry_after_max: Duration::from_secs(1),
    }
}

#[tokio::test]
async fn sends_documented_request_shape_and_reads_typed_answers() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/systemone"))
        .and(header("authorization", "Bearer test-key"))
        .and(header("content-type", "application/json"))
        .and(body_partial_json(json!({
            "model": "jev-latest",
            "state": { "message": "charged twice" },
            "questions": {
                "dept": { "type": "choice", "criteria": { "billing": "Money", "technical": "Bugs" } },
                "urgent": { "type": "noul" }
            }
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "model": "jev-1.13.0",
            "answers": {
                "dept": { "type": "choice", "choice": "billing",
                          "probabilities": { "billing": 0.88, "technical": 0.12 }, "confidence": 0.81 },
                "urgent": { "type": "noul", "noul": 0.95 }
            },
            "usage": { "input_tokens": 296, "output_tokens": 20 }
        })))
        .expect(1)
        .mount(&server)
        .await;

    let mut q = Questions::new();
    let dept = q.choice::<Department>("dept", "Which team?").unwrap();
    let urgent = q.noul("urgent", "Urgent?", None).unwrap();

    let c = client(&server, RetryPolicy::none());
    let response = c
        .system_one(&json!({ "message": "charged twice" }), &q)
        .await
        .unwrap();

    assert_eq!(response.model, "jev-1.13.0");
    assert_eq!(response.usage.input_tokens, 296);
    let dept = response.get(&dept).unwrap();
    assert_eq!(dept.chosen, Department::Billing);
    assert!(dept.confidence.at_least(0.8));
    assert!(response.get(&urgent).unwrap().is_yes(0.9));
}

#[tokio::test]
async fn maps_401_and_422_without_retrying() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(401))
        .expect(1)
        .mount(&server)
        .await;
    let c = client(&server, fast_retries(3));
    let mut q = Questions::new();
    q.noul("x", "?", None).unwrap();
    assert!(matches!(
        c.system_one(&"s", &q).await,
        Err(Error::Unauthorized)
    ));
    server.reset().await;

    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(422)
                .set_body_string(r#"{"detail":"questions.x.criteria: invalid"}"#),
        )
        .expect(1)
        .mount(&server)
        .await;
    match c.system_one(&"s", &q).await {
        Err(Error::InvalidRequest { detail }) => assert!(detail.contains("questions.x.criteria")),
        other => panic!("unexpected: {other:?}"),
    }
}

#[tokio::test]
async fn retries_429_honouring_retry_after_then_succeeds() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "0"))
        .up_to_n_times(2)
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "model": "jev-1.13.0",
            "answers": { "x": { "type": "noul", "noul": 0.5 } },
            "usage": { "input_tokens": 1, "output_tokens": 1 }
        })))
        .expect(1)
        .mount(&server)
        .await;

    let c = client(&server, fast_retries(2));
    let mut q = Questions::new();
    let x = q.noul("x", "?", None).unwrap();
    let r = c.system_one(&"s", &q).await.unwrap();
    assert!((r.get(&x).unwrap().yes.value() - 0.5).abs() < f64::EPSILON);
}

#[tokio::test]
async fn exhausted_retries_report_attempt_count() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(529))
        .expect(3)
        .mount(&server)
        .await;
    let c = client(&server, fast_retries(2));
    let mut q = Questions::new();
    q.noul("x", "?", None).unwrap();
    assert!(matches!(
        c.system_one(&"s", &q).await,
        Err(Error::Overloaded { attempts: 3 })
    ));
}

#[tokio::test]
async fn lists_models() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "models": [
                { "name": "jev-latest", "description": "Stable", "release_date": "2026-06-01" }
            ]
        })))
        .mount(&server)
        .await;
    let models = client(&server, RetryPolicy::none())
        .list_models()
        .await
        .unwrap();
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].name, "jev-latest");
}

#[tokio::test]
async fn triage_end_to_end_attaches_to_duplicate_incident() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/systemone"))
        // The dedup Choice must offer the incident ids plus `none`.
        .and(body_partial_json(json!({
            "questions": { "duplicate_of": { "type": "choice",
                "criteria": { "INC-4821": "Checkout 5xx spike", "none": "`alert` is a new, separate problem not covered by any open incident" } } }
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "model": "jev-1.13.0",
            "answers": {
                "owner": { "type": "choice", "choice": "application",
                           "probabilities": { "application": 0.7, "platform": 0.3 }, "confidence": 0.6 },
                "impact": { "type": "score", "score": 2.1,
                            "legend": { "0": "a", "1": "b", "2": "c", "3": "d" },
                            "probabilities": { "0": 0.0, "1": 0.1, "2": 0.7, "3": 0.2 }, "confidence": 0.6 },
                "actionable": { "type": "noul", "noul": 0.93 },
                "duplicate_of": { "type": "choice", "choice": "INC-4821",
                                  "probabilities": { "INC-4821": 0.92, "none": 0.08 }, "confidence": 0.84 }
            },
            "usage": { "input_tokens": 500, "output_tokens": 40 }
        })))
        .expect(1)
        .mount(&server)
        .await;

    let alert = Alert {
        source: "prometheus".into(),
        title: "HighErrorRate".into(),
        description: "checkout-api 5xx ratio 12% for 10m".into(),
        labels: [("service".to_owned(), "checkout-api".to_owned())].into(),
        runbook: None,
        recent_changes: vec![],
        open_incidents: vec![OpenIncident {
            id: "INC-4821".into(),
            summary: "Checkout 5xx spike".into(),
        }],
        component: None,
        related_alerts: vec![],
    };
    let questions = TriageQuestions::for_alert(&alert).unwrap();
    let c = client(&server, RetryPolicy::none());
    let response = c
        .system_one(&TriageQuestions::state(&alert), &questions.questions)
        .await
        .unwrap();
    let answers = questions.read(&response).unwrap();
    assert_eq!(answers.owner.chosen, "application");

    let decision = decide(&answers, &Policy::default());
    assert_eq!(
        decision,
        Decision::AttachToIncident {
            incident_id: "INC-4821".into(),
            confidence: 0.84
        }
    );
}
