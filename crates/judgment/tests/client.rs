//! Client behaviour against a mock TypeSafe API: the documented request shape
//! and typed answers, error mapping, retry with `Retry-After`, exhausted
//! retries, the models list, and the request id carried from the
//! `x-typesafe-request-id` header onto responses and errors.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Duration;

use judgment::client::REQUEST_ID_HEADER;
use judgment::{Client, Error, Questions, Recorder, Replay, RetryPolicy, SystemOne, options};
use serde_json::json;
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
        Err(Error::Unauthorized { .. })
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
        Err(Error::InvalidRequest { detail, .. }) => {
            assert!(detail.contains("questions.x.criteria"));
        }
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
        Err(Error::Overloaded { attempts: 3, .. })
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

/// `Client` is a `SystemOne`: the same request through the trait object.
#[tokio::test]
async fn the_client_answers_through_the_backend_trait() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/systemone"))
        .and(body_partial_json(
            json!({ "model": "jev-latest", "state": { "message": "hi" } }),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "model": "jev-1.13.0",
            "answers": { "urgent": { "type": "noul", "noul": 0.9 } },
            "usage": { "input_tokens": 3, "output_tokens": 1 }
        })))
        .expect(1)
        .mount(&server)
        .await;
    let mut q = Questions::new();
    let urgent = q.noul("urgent", "Is `message` urgent?", None).unwrap();
    let backend: &dyn SystemOne = &client(&server, RetryPolicy::none());
    let response = backend
        .answer(&json!({ "message": "hi" }), "jev-latest", &q)
        .await
        .unwrap();
    assert!(response.get(&urgent).unwrap().is_yes(0.5));
}

fn noul_body() -> serde_json::Value {
    json!({
        "model": "jev-1.13.0",
        "answers": { "x": { "type": "noul", "noul": 0.5 } },
        "usage": { "input_tokens": 1, "output_tokens": 1 }
    })
}

fn one_noul() -> Questions {
    let mut q = Questions::new();
    q.noul("x", "?", None).unwrap();
    q
}

#[tokio::test]
async fn a_response_carries_the_request_id_header() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header(REQUEST_ID_HEADER, "req_ok")
                .set_body_json(noul_body()),
        )
        .expect(1)
        .mount(&server)
        .await;
    let c = client(&server, RetryPolicy::none());
    let q = one_noul();
    let r = c.system_one(&"s", &q).await.unwrap();
    assert_eq!(r.request_id.as_deref(), Some("req_ok"));
    server.reset().await;

    // The same response without the header; a `request_id` key in the body
    // does not stand in for it.
    let mut body = noul_body();
    body["request_id"] = json!("from-the-body");
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .expect(1)
        .mount(&server)
        .await;
    let r = c.system_one(&"s", &q).await.unwrap();
    assert_eq!(r.request_id, None);
}

#[tokio::test]
async fn every_http_error_carries_the_request_id_in_its_value_and_message() {
    let server = MockServer::start().await;
    let c = client(&server, RetryPolicy::none());
    let q = one_noul();
    for status in [401_u16, 422, 429, 529, 404, 500] {
        let id = format!("req_{status}");
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(status)
                    .insert_header(REQUEST_ID_HEADER, id.as_str())
                    .set_body_string("{\"detail\":\"nope\"}"),
            )
            .expect(1)
            .mount(&server)
            .await;
        let err = c.system_one(&"s", &q).await.unwrap_err();
        let variant_fits = match status {
            401 => matches!(err, Error::Unauthorized { .. }),
            422 => matches!(err, Error::InvalidRequest { .. }),
            429 => matches!(err, Error::RateLimited { attempts: 1, .. }),
            529 => matches!(err, Error::Overloaded { attempts: 1, .. }),
            _ => matches!(err, Error::Http { status: s, .. } if s == status),
        };
        assert!(variant_fits, "{status}: {err:?}");
        assert_eq!(err.request_id(), Some(id.as_str()), "{status}");
        let message = err.to_string();
        assert!(
            message.ends_with(&format!(" [request_id {id}]")),
            "{status}: {message}"
        );
        server.reset().await;
    }
}

#[tokio::test]
async fn an_error_without_the_header_reads_as_before() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(401))
        .expect(1)
        .mount(&server)
        .await;
    let err = client(&server, RetryPolicy::none())
        .system_one(&"s", &one_noul())
        .await
        .unwrap_err();
    assert!(
        matches!(err, Error::Unauthorized { request_id: None }),
        "{err:?}"
    );
    assert_eq!(
        err.to_string(),
        "authentication failed (401): check the API key"
    );
}

#[tokio::test]
async fn the_request_id_is_the_last_attempts() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(529).insert_header(REQUEST_ID_HEADER, "req_first"))
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header(REQUEST_ID_HEADER, "req_second")
                .set_body_json(noul_body()),
        )
        .expect(1)
        .mount(&server)
        .await;
    let c = client(&server, fast_retries(1));
    let q = one_noul();
    let r = c.system_one(&"s", &q).await.unwrap();
    assert_eq!(r.request_id.as_deref(), Some("req_second"));
    server.reset().await;

    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(529).insert_header(REQUEST_ID_HEADER, "req_a"))
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(529).insert_header(REQUEST_ID_HEADER, "req_b"))
        .expect(1)
        .mount(&server)
        .await;
    let err = c.system_one(&"s", &q).await.unwrap_err();
    assert!(
        matches!(
            &err,
            Error::Overloaded { attempts: 2, request_id: Some(id) } if id == "req_b"
        ),
        "{err:?}"
    );
}

#[tokio::test]
async fn a_body_that_does_not_decode_keeps_the_request_id() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header(REQUEST_ID_HEADER, "req_html")
                .set_body_string("<html>not the API</html>"),
        )
        // Once: a 2xx that does not decode is not retried.
        .expect(1)
        .mount(&server)
        .await;
    let err = client(&server, fast_retries(3))
        .system_one(&"s", &one_noul())
        .await
        .unwrap_err();
    assert!(
        matches!(&err, Error::Decode { request_id: Some(id), .. } if id == "req_html"),
        "{err:?}"
    );
    assert!(err.to_string().ends_with(" [request_id req_html]"), "{err}");
}

#[tokio::test]
async fn the_models_list_carries_the_request_id_on_errors() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(404).insert_header(REQUEST_ID_HEADER, "req_models"))
        .expect(1)
        .mount(&server)
        .await;
    let err = client(&server, RetryPolicy::none())
        .list_models()
        .await
        .unwrap_err();
    assert!(
        matches!(
            &err,
            Error::Http { status: 404, request_id: Some(id), .. } if id == "req_models"
        ),
        "{err:?}"
    );
}

#[tokio::test]
async fn a_transport_failure_has_no_request_id() {
    // Port 1 is reserved and nothing listens there: the connection is
    // refused before any response exists.
    let c = Client::builder()
        .api_key("test-key")
        .base_url("http://127.0.0.1:1")
        .retry(RetryPolicy::none())
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap();
    let err = c.system_one(&"s", &one_noul()).await.unwrap_err();
    assert!(matches!(err, Error::Transport { .. }), "{err:?}");
    assert_eq!(err.request_id(), None);
}

#[tokio::test]
async fn a_recording_keeps_the_request_id_and_a_replay_returns_it() {
    let dir = std::env::temp_dir().join(format!(
        "judgment-a_recording_keeps_the_request_id_and_a_replay_returns_it-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header(REQUEST_ID_HEADER, "req_rec")
                .set_body_json(noul_body()),
        )
        .expect(1)
        .mount(&server)
        .await;
    let q = one_noul();
    let state = json!({ "message": "hi" });

    let recorder = Recorder::new(client(&server, RetryPolicy::none()), &dir);
    let live = recorder.answer(&state, "jev-latest", &q).await.unwrap();
    assert_eq!(live.request_id.as_deref(), Some("req_rec"));
    let files: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(files.len(), 1, "{files:?}");
    let text = std::fs::read_to_string(&files[0]).unwrap();
    assert!(text.contains(r#""request_id": "req_rec""#), "{text}");

    let replayed = Replay::open(&dir)
        .unwrap()
        .answer(&state, "jev-latest", &q)
        .await
        .unwrap();
    assert_eq!(replayed, live);
    assert_eq!(replayed.request_id.as_deref(), Some("req_rec"));
    let _ = std::fs::remove_dir_all(&dir);
}
