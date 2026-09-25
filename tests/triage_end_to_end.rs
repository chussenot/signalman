//! The triage question set end to end against a mock TypeSafe API: the dedup
//! Choice offers the open incidents plus `none`, and the policy attaches the
//! alert to the incident the model chose.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::time::Duration;

use serde_json::json;
use signalman::triage::{Alert, Decision, OpenIncident, Policy, TriageQuestions, decide};
use signalman::{Client, RetryPolicy};
use wiremock::matchers::{body_partial_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn client(server: &MockServer, retry: RetryPolicy) -> Client {
    Client::builder()
        .api_key("test-key")
        .base_url(server.uri())
        .retry(retry)
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap()
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
                            "legend": common::impact_legend(),
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
