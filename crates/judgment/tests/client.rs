//! Client behaviour against a mock TypeSafe API: the documented request shape
//! and typed answers, error mapping by remedy (400 and 422 parsed into
//! issues, 403 apart from 401, 404 left as an HTTP error), the trimmed key,
//! retries (the server's wait from `retry-after-ms` or `Retry-After`, the
//! budget, `RetryPolicy::conservative`, the transport levels against a
//! refused connection, a timeout and a truncated body), exhausted retries,
//! the models list, and the request id carried from the
//! `x-typesafe-request-id` header onto responses and errors.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use judgment::client::REQUEST_ID_HEADER;
use judgment::{
    Client, Error, Questions, Recorder, Replay, RetryPolicy, SystemOne, TransportRetry, options,
};
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
        ..RetryPolicy::default()
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

/// The OpenAPI document's `HTTPValidationError` example, at
/// `/components/schemas/HTTPValidationError/properties/detail/examples/0`.
const VALIDATION_EXAMPLE: &str =
    r#"{"detail":[{"loc":["body","state"],"msg":"Field required","type":"missing"}]}"#;

#[tokio::test]
async fn maps_4xx_by_remedy_without_retrying() {
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
    // `reset` drops the mounted mocks without checking them, so each
    // `expect(1)` is verified before it: a retried 4xx would be sent four
    // times under `fast_retries(3)` and fail here.
    server.verify().await;
    server.reset().await;

    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(422).set_body_string(VALIDATION_EXAMPLE))
        .expect(1)
        .mount(&server)
        .await;
    match c.system_one(&"s", &q).await {
        Err(Error::InvalidRequest {
            status: 422,
            detail,
            issues,
            ..
        }) => {
            assert_eq!(detail, "state: Field required");
            assert_eq!(issues.len(), 1);
            assert_eq!(issues[0].path(), "state");
            assert_eq!(issues[0].kind, "missing");
            assert_eq!(issues[0].msg, "Field required");
        }
        other => panic!("unexpected: {other:?}"),
    }
    server.verify().await;
    server.reset().await;

    // A string `detail` is the message, with no issues to parse.
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(422)
                .set_body_string(r#"{"detail":"questions.x.criteria: invalid"}"#),
        )
        .expect(1)
        .mount(&server)
        .await;
    match c.system_one(&"s", &q).await {
        Err(Error::InvalidRequest {
            status: 422,
            detail,
            issues,
            ..
        }) => {
            assert_eq!(detail, "questions.x.criteria: invalid");
            assert!(issues.is_empty(), "{issues:?}");
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[tokio::test]
async fn a_400_is_an_invalid_request_with_the_servers_message() {
    let server = MockServer::start().await;
    let c = client(&server, fast_retries(3));
    let q = one_noul();
    let bodies = [
        (r#"{"error":"model mismatch"}"#, "model mismatch", 0),
        // What examples/laya/serve_laya.py answers when Laya raises.
        (
            r#"{"error":{"message":"'instructions'","type":"invalid_request"}}"#,
            "'instructions'",
            0,
        ),
        // FastAPI's `detail`, as a list and as a string.
        (VALIDATION_EXAMPLE, "state: Field required", 1),
        (r#"{"detail":"malformed body"}"#, "malformed body", 0),
    ];
    for (body, message, issue_count) in bodies {
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(400).set_body_string(body))
            // Once: a 400 is not retried.
            .expect(1)
            .mount(&server)
            .await;
        let err = c.system_one(&"s", &q).await.unwrap_err();
        match &err {
            Error::InvalidRequest {
                status: 400,
                detail,
                issues,
                ..
            } => {
                assert_eq!(detail, message, "{body}");
                assert_eq!(issues.len(), issue_count, "{body}");
            }
            other => panic!("{body}: unexpected {other:?}"),
        }
        assert_eq!(
            err.to_string(),
            format!("request rejected by the API (400): {message}")
        );
        // `reset` does not check expectations; verify the `expect(1)` first.
        server.verify().await;
        server.reset().await;
    }
}

#[tokio::test]
async fn a_403_is_permission_denied_and_not_retried() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(403)
                .insert_header(REQUEST_ID_HEADER, "req_403")
                .set_body_string(r#"{"error":{"message":"model not enabled for this account"}}"#),
        )
        .expect(1)
        .mount(&server)
        .await;
    let err = client(&server, fast_retries(3))
        .system_one(&"s", &one_noul())
        .await
        .unwrap_err();
    assert!(
        matches!(&err, Error::PermissionDenied { detail, .. } if detail == "model not enabled for this account"),
        "{err:?}"
    );
    assert_eq!(err.request_id(), Some("req_403"));
    assert_eq!(
        err.to_string(),
        "permission denied (403): model not enabled for this account [request_id req_403]"
    );
}

#[tokio::test]
async fn a_404_stays_an_http_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(404).set_body_string(r#"{"detail":"Not Found"}"#))
        .expect(1)
        .mount(&server)
        .await;
    let err = client(&server, fast_retries(3))
        .list_models()
        .await
        .unwrap_err();
    assert!(
        matches!(&err, Error::Http { status: 404, body, .. } if body == r#"{"detail":"Not Found"}"#),
        "{err:?}"
    );
}

#[tokio::test]
async fn a_422_that_is_not_json_keeps_the_body_as_detail() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(422).set_body_string("<html>bad request</html>"))
        .expect(1)
        .mount(&server)
        .await;
    let err = client(&server, RetryPolicy::none())
        .system_one(&"s", &one_noul())
        .await
        .unwrap_err();
    assert!(
        matches!(
            &err,
            Error::InvalidRequest { status: 422, detail, issues, .. }
                if detail == "<html>bad request</html>" && issues.is_empty()
        ),
        "{err:?}"
    );
}

#[tokio::test]
async fn the_trimmed_key_is_the_one_sent() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(header("authorization", "Bearer test-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(noul_body()))
        .expect(1)
        .mount(&server)
        .await;
    let c = Client::builder()
        .api_key(" test-key\r\n")
        .base_url(server.uri())
        .retry(RetryPolicy::none())
        .build()
        .unwrap();
    c.system_one(&"s", &one_noul()).await.unwrap();
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
async fn retry_after_ms_is_honoured_before_retry_after() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("retry-after-ms", "250")
                .insert_header("retry-after", "0"),
        )
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(noul_body()))
        .expect(1)
        .mount(&server)
        .await;
    let c = client(&server, fast_retries(1));
    let started = Instant::now();
    c.system_one(&"s", &one_noul()).await.unwrap();
    // Only a lower bound: `Retry-After: 0` alone would have retried at once.
    let elapsed = started.elapsed();
    assert!(elapsed >= Duration::from_millis(250), "{elapsed:?}");
}

#[tokio::test]
async fn budget_stops_before_a_wait_that_would_reach_it() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(529))
        .expect(2)
        .mount(&server)
        .await;
    let c = client(
        &server,
        RetryPolicy {
            max_retries: 5,
            backoff_initial: Duration::from_millis(100),
            backoff_max: Duration::from_secs(1),
            backoff_jitter: 0.0,
            budget: Some(Duration::from_millis(250)),
            ..RetryPolicy::default()
        },
    );
    // First wait: about 0 + 100 ms, under the budget, taken. Second wait:
    // at least 100 + 200 ms, which reaches 250 ms, so the loop stops there.
    let err = c.system_one(&"s", &one_noul()).await.unwrap_err();
    assert!(
        matches!(err, Error::Overloaded { attempts: 2, .. }),
        "{err:?}"
    );
}

#[tokio::test]
async fn retry_after_beyond_the_budget_is_not_waited() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "1"))
        .expect(1)
        .mount(&server)
        .await;
    let c = client(
        &server,
        RetryPolicy {
            backoff_initial: Duration::from_millis(100),
            backoff_jitter: 0.0,
            budget: Some(Duration::from_millis(500)),
            ..RetryPolicy::default()
        },
    );
    // The server asks for 1 s and only 500 ms are left: the failure comes
    // back now, with the server's wait for the caller to honour.
    let err = c.system_one(&"s", &one_noul()).await.unwrap_err();
    assert!(
        matches!(
            err,
            Error::RateLimited {
                attempts: 1,
                retry_after: Some(d),
                ..
            } if d == Duration::from_secs(1)
        ),
        "{err:?}"
    );
}

/// [`RetryPolicy::conservative`] with waits short enough for a test.
fn fast_conservative() -> RetryPolicy {
    RetryPolicy {
        backoff_initial: Duration::from_millis(5),
        backoff_max: Duration::from_millis(20),
        backoff_jitter: 0.0,
        ..RetryPolicy::conservative()
    }
}

fn client_at(base_url: &str, retry: RetryPolicy, timeout: Duration) -> Client {
    Client::builder()
        .api_key("test-key")
        .base_url(base_url)
        .retry(retry)
        .timeout(timeout)
        .build()
        .unwrap()
}

#[tokio::test]
async fn conservative_does_not_retry_5xx() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(503))
        .expect(1)
        .mount(&server)
        .await;
    let err = client(&server, fast_conservative())
        .system_one(&"s", &one_noul())
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Http { status: 503, .. }), "{err:?}");
}

#[tokio::test]
async fn conservative_retries_429() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(429))
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(noul_body()))
        .expect(1)
        .mount(&server)
        .await;
    client(&server, fast_conservative())
        .system_one(&"s", &one_noul())
        .await
        .unwrap();
}

#[tokio::test]
async fn conservative_does_not_retry_a_timeout() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(noul_body())
                .set_delay(Duration::from_millis(500)),
        )
        .expect(1)
        .mount(&server)
        .await;
    // The request was sent before the timeout fired, so it may have been
    // processed and billed: not retried.
    let err = client_at(
        &server.uri(),
        fast_conservative(),
        Duration::from_millis(50),
    )
    .system_one(&"s", &one_noul())
    .await
    .unwrap_err();
    assert!(
        matches!(&err, Error::Transport { attempts: 1, source } if source.is_timeout()),
        "{err:?}"
    );
}

/// A base URL nothing listens on: an ephemeral port bound and released, so
/// a connect is refused before anything is sent. Another process could take
/// the port between the release and the connect; that race is rare and
/// accepted.
fn refused_url() -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    format!("http://{addr}")
}

#[tokio::test]
async fn conservative_retries_a_refused_connection() {
    let err = client_at(&refused_url(), fast_conservative(), Duration::from_secs(2))
        .system_one(&"s", &one_noul())
        .await
        .unwrap_err();
    assert!(
        matches!(&err, Error::Transport { attempts: 3, source } if source.is_connect()),
        "{err:?}"
    );
}

#[tokio::test]
async fn transport_never_does_not_retry() {
    let never = RetryPolicy {
        transport: TransportRetry::Never,
        ..fast_retries(2)
    };
    let err = client_at(&refused_url(), never, Duration::from_secs(2))
        .system_one(&"s", &one_noul())
        .await
        .unwrap_err();
    assert!(
        matches!(err, Error::Transport { attempts: 1, .. }),
        "{err:?}"
    );
}

/// A raw HTTP/1.1 server whose every response promises a 100-byte body and
/// closes after 9 bytes of it, counting the connections it accepts.
/// wiremock cannot truncate a body, hence a thread over a std listener. Its
/// accept loop polls, so it stops when `stop` is set or after 5 s, and a
/// regression fails the test instead of hanging it.
fn truncating_server() -> (String, Arc<AtomicUsize>, Arc<AtomicBool>, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let connections = Arc::new(AtomicUsize::new(0));
    let stop = Arc::new(AtomicBool::new(false));
    let (count, done) = (Arc::clone(&connections), Arc::clone(&stop));
    let thread = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !done.load(Ordering::SeqCst) && Instant::now() < deadline {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    count.fetch_add(1, Ordering::SeqCst);
                    stream.set_nonblocking(false).unwrap();
                    stream
                        .set_read_timeout(Some(Duration::from_secs(5)))
                        .unwrap();
                    read_request(&mut stream);
                    let _ = stream.write_all(
                        b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n\
                          content-length: 100\r\n\r\n{\"model\":",
                    );
                    let _ = stream.flush();
                    let _ = stream.shutdown(Shutdown::Both);
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(e) => panic!("accept: {e}"),
            }
        }
    });
    (url, connections, stop, thread)
}

/// Read one request, head and body, so that closing the connection sends a
/// clean end of stream rather than a reset over unread bytes.
fn read_request(stream: &mut TcpStream) {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let mut wanted = None;
    loop {
        if let Some(total) = wanted
            && buf.len() >= total
        {
            return;
        }
        let n = match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return,
            Ok(n) => n,
        };
        buf.extend_from_slice(&chunk[..n]);
        if wanted.is_none()
            && let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n")
        {
            let head = String::from_utf8_lossy(&buf[..end]).to_ascii_lowercase();
            let body = head
                .lines()
                .find_map(|l| l.strip_prefix("content-length:"))
                .and_then(|v| v.trim().parse::<usize>().ok())
                .unwrap_or(0);
            wanted = Some(end + 4 + body);
        }
    }
}

#[tokio::test]
async fn a_truncated_body_is_retried_by_default_only() {
    for (transport, attempts) in [(TransportRetry::BeforeSend, 1), (TransportRetry::Any, 3)] {
        let (url, connections, stop, thread) = truncating_server();
        let policy = RetryPolicy {
            transport,
            ..fast_retries(2)
        };
        let err = client_at(&url, policy, Duration::from_secs(2))
            .system_one(&"s", &one_noul())
            .await
            .unwrap_err();
        stop.store(true, Ordering::SeqCst);
        thread.join().unwrap();
        assert!(
            matches!(&err, Error::Transport { attempts: a, .. } if *a == attempts),
            "{transport:?}: {err:?}"
        );
        assert_eq!(
            connections.load(Ordering::SeqCst),
            attempts as usize,
            "{transport:?}"
        );
    }
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
    for status in [400_u16, 401, 403, 422, 429, 529, 404, 500] {
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
            400 => matches!(err, Error::InvalidRequest { status: 400, .. }),
            401 => matches!(err, Error::Unauthorized { .. }),
            403 => matches!(err, Error::PermissionDenied { .. }),
            422 => matches!(err, Error::InvalidRequest { status: 422, .. }),
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
