//! Client behaviour against a mock TypeSafe API: the documented request shape
//! and typed answers, error mapping by remedy (400 and 422 parsed into
//! issues, 403 apart from 401, 404 left as an HTTP error), the trimmed key,
//! retries (the server's wait from `retry-after-ms` or `Retry-After`, the
//! budget, `RetryPolicy::conservative`, a listed 2xx sent once, the transport
//! levels against a refused connection, a timeout and a truncated body),
//! exhausted retries, the models list, the request id carried from the
//! `x-typesafe-request-id` header onto responses and errors, and per-call
//! options (timeout, retry policy, headers, extra body fields) beside the
//! builder's default headers, the tolerant decoding of an answer kind this
//! release does not know, undocumented fields and a missing `usage`, and the
//! refusal, without a retry, of a response that does not answer the
//! questions it was sent.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use judgment::client::{HeaderName, HeaderValue, REQUEST_ID_HEADER};
use judgment::{
    Answer, CallOptions, Client, Error, Observer, Questions, Recorder, Replay, Request,
    RetryPolicy, SystemOne, TransportRetry, Usage, options,
};
use serde_json::json;
use wiremock::matchers::{body_json, body_partial_json, header, method, path};
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

#[tokio::test]
async fn a_success_in_the_status_set_is_not_sent_again() {
    // A "retry everything" set lists the 2xx too. A System One call is
    // billed, so a success must come back after one send, never re-sent
    // until the retries run out.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "model": "jev-1.13.0",
            "answers": { "x": { "type": "noul", "noul": 0.75 } },
            "usage": { "input_tokens": 10, "output_tokens": 1 }
        })))
        .expect(1)
        .mount(&server)
        .await;
    let everything = RetryPolicy {
        http_statuses: (200..=599).collect(),
        ..fast_retries(2)
    };
    let response = client(&server, everything)
        .system_one(&"s", &one_noul())
        .await
        .unwrap();
    assert_eq!(response.model, "jev-1.13.0");
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

/// The body is the fixture `tests/contract.rs` checks against the OpenAPI
/// document's `ModelMetadataList`, so this mock is a list the schema allows.
#[tokio::test]
async fn lists_models() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(include_str!("fixtures/models.json"), "application/json"),
        )
        .mount(&server)
        .await;
    let models = client(&server, RetryPolicy::none())
        .list_models()
        .await
        .unwrap();
    let names: Vec<&str> = models.iter().map(|m| m.name.as_str()).collect();
    assert_eq!(names, ["jev-latest", "jev-1.13.0"]);
    assert_eq!(models[1].release_date, "2026-09-15");
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

/// A header name for a test; lowercase, as `from_static` requires.
fn name(n: &'static str) -> HeaderName {
    HeaderName::from_static(n)
}

fn value(v: &'static str) -> HeaderValue {
    HeaderValue::from_static(v)
}

/// The body [`one_noul`] and a string state make, as the OpenAPI document
/// spells it.
fn one_noul_body(state: &str) -> serde_json::Value {
    json!({
        "state": state,
        "model": "jev-latest",
        "questions": { "x": { "type": "noul", "instructions": "?" } }
    })
}

#[tokio::test]
async fn the_documented_body_is_all_that_is_sent_without_options() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/systemone"))
        .and(header("authorization", "Bearer test-key"))
        .and(header("content-type", "application/json"))
        .and(body_json(one_noul_body("s")))
        .respond_with(ResponseTemplate::new(200).set_body_json(noul_body()))
        .expect(3)
        .mount(&server)
        .await;
    let c = client(&server, RetryPolicy::none());
    let q = one_noul();
    let request = Request {
        state: &"s",
        model: "jev-latest",
        questions: &q,
    };
    c.evaluate(&request).await.unwrap();
    c.evaluate_with(&request, &CallOptions::default())
        .await
        .unwrap();
    c.system_one(&"s", &q).await.unwrap();

    // The three bodies are the same bytes, not only the same JSON.
    let received = server.received_requests().await.unwrap();
    assert_eq!(received.len(), 3);
    assert!(
        received.iter().all(|r| r.body == received[0].body),
        "{:?}",
        received
            .iter()
            .map(|r| String::from_utf8_lossy(&r.body).into_owned())
            .collect::<Vec<_>>()
    );
    assert_eq!(received[0].body, serde_json::to_vec(&request).unwrap());
}

#[tokio::test]
async fn the_client_sets_no_header_it_does_not_reserve() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(noul_body()))
        .expect(1)
        .mount(&server)
        .await;
    client(&server, RetryPolicy::none())
        .system_one(&"s", &one_noul())
        .await
        .unwrap();
    let received = server.received_requests().await.unwrap();
    // The four reserved headers (the retry count is reserved but not sent),
    // and what HTTP itself adds.
    let allowed = [
        "authorization",
        "content-type",
        "user-agent",
        "x-typesafe-retry-count",
        "host",
        "content-length",
        "accept",
    ];
    let sent: Vec<&str> = received[0].headers.keys().map(HeaderName::as_str).collect();
    for n in &sent {
        assert!(allowed.contains(n), "unexpected header {n:?} in {sent:?}");
    }
    assert!(sent.contains(&"authorization") && sent.contains(&"user-agent"));
    assert!(!sent.contains(&"x-typesafe-retry-count"), "not sent yet");
}

#[tokio::test]
async fn extra_fields_are_sent_beside_the_documented_ones() {
    let server = MockServer::start().await;
    let mut expected = one_noul_body("s");
    expected["beam_width"] = json!(4);
    expected["tag"] = json!(null);
    Mock::given(method("POST"))
        .and(body_json(expected))
        .respond_with(ResponseTemplate::new(200).set_body_json(noul_body()))
        .expect(1)
        .mount(&server)
        .await;
    let q = one_noul();
    let options = CallOptions::new()
        .extra("beam_width", 4)
        .unwrap()
        .extra("tag", serde_json::Value::Null)
        .unwrap();
    client(&server, RetryPolicy::none())
        .evaluate_with(
            &Request {
                state: &"s",
                model: "jev-latest",
                questions: &q,
            },
            &options,
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn call_headers_replace_builder_defaults_and_never_the_key() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(header("authorization", "Bearer test-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(noul_body()))
        .expect(2)
        .mount(&server)
        .await;
    let c = Client::builder()
        .api_key("test-key")
        .base_url(server.uri())
        .retry(RetryPolicy::none())
        .default_header(name("x-team"), value("builder"))
        .default_header(name("x-tenant"), value("acme"))
        .build()
        .unwrap();
    let q = one_noul();
    let request = Request {
        state: &"s",
        model: "jev-latest",
        questions: &q,
    };
    c.evaluate(&request).await.unwrap();
    let options = CallOptions::new()
        .header(name("x-team"), value("per-call"))
        .unwrap();
    c.evaluate_with(&request, &options).await.unwrap();

    let received = server.received_requests().await.unwrap();
    let values = |i: usize, n: &str| -> Vec<String> {
        received[i]
            .headers
            .get_all(n)
            .iter()
            .map(|v| v.to_str().unwrap().to_owned())
            .collect()
    };
    assert_eq!(values(0, "x-team"), ["builder"]);
    assert_eq!(values(1, "x-team"), ["per-call"], "replaced, not appended");
    assert_eq!(values(1, "x-tenant"), ["acme"], "other defaults stay");
    for i in 0..2 {
        assert_eq!(values(i, "authorization"), ["Bearer test-key"]);
        assert_eq!(values(i, "content-type"), ["application/json"]);
    }

    // The key cannot be replaced for one call.
    let err = CallOptions::new()
        .header(name("authorization"), value("Bearer other"))
        .unwrap_err();
    assert!(matches!(err, Error::ReservedHeader(_)), "{err:?}");
}

#[tokio::test]
async fn per_call_headers_and_extras_reach_every_attempt() {
    let server = MockServer::start().await;
    let mut expected = one_noul_body("s");
    expected["beam_width"] = json!(4);
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&calls);
    Mock::given(method("POST"))
        .and(header("x-team", "per-call"))
        .and(body_json(expected))
        .respond_with(move |_: &wiremock::Request| {
            if seen.fetch_add(1, Ordering::SeqCst) == 0 {
                ResponseTemplate::new(529)
            } else {
                ResponseTemplate::new(200).set_body_json(noul_body())
            }
        })
        .expect(2)
        .mount(&server)
        .await;
    let q = one_noul();
    let options = CallOptions::new()
        .header(name("x-team"), value("per-call"))
        .unwrap()
        .extra("beam_width", 4)
        .unwrap();
    client(&server, fast_retries(2))
        .evaluate_with(
            &Request {
                state: &"s",
                model: "jev-latest",
                questions: &q,
            },
            &options,
        )
        .await
        .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn the_client_behind_the_trait_sends_its_default_headers() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(header("x-team", "builder"))
        .and(body_json(one_noul_body("s")))
        .respond_with(ResponseTemplate::new(200).set_body_json(noul_body()))
        .expect(1)
        .mount(&server)
        .await;
    let c = Client::builder()
        .api_key("test-key")
        .base_url(server.uri())
        .retry(RetryPolicy::none())
        .default_header(name("x-team"), value("builder"))
        .build()
        .unwrap();
    let backend: &dyn SystemOne = &c;
    backend
        .answer(&json!("s"), "jev-latest", &one_noul())
        .await
        .unwrap();
}

#[tokio::test]
async fn builder_default_headers_reach_list_models() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .and(header("x-team", "builder"))
        .and(header("authorization", "Bearer test-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "models": [] })))
        .expect(1)
        .mount(&server)
        .await;
    let models = Client::builder()
        .api_key("test-key")
        .base_url(server.uri())
        .retry(RetryPolicy::none())
        .default_header(name("x-team"), value("builder"))
        .build()
        .unwrap()
        .list_models()
        .await
        .unwrap();
    assert!(models.is_empty());
}

#[tokio::test]
async fn a_call_timeout_replaces_the_per_attempt_timeout() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(noul_body())
                .set_delay(Duration::from_millis(500)),
        )
        .expect(2)
        .mount(&server)
        .await;
    let c = client_at(&server.uri(), RetryPolicy::none(), Duration::from_secs(2));
    let q = one_noul();
    let request = Request {
        state: &"s",
        model: "jev-latest",
        questions: &q,
    };
    let short = CallOptions::new().timeout(Duration::from_millis(50));
    let err = c.evaluate_with(&request, &short).await.unwrap_err();
    assert!(
        matches!(&err, Error::Transport { attempts: 1, source } if source.is_timeout()),
        "{err:?}"
    );
    // The client's own 2 s still applies to a call without options.
    c.evaluate(&request).await.unwrap();
}

#[tokio::test]
async fn a_longer_call_timeout_is_not_capped_by_the_builder() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(noul_body())
                .set_delay(Duration::from_millis(300)),
        )
        .expect(1)
        .mount(&server)
        .await;
    let c = client_at(
        &server.uri(),
        RetryPolicy::none(),
        Duration::from_millis(50),
    );
    let q = one_noul();
    let long = CallOptions::new().timeout(Duration::from_secs(2));
    c.evaluate_with(
        &Request {
            state: &"s",
            model: "jev-latest",
            questions: &q,
        },
        &long,
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn a_call_retry_policy_replaces_the_client_policy() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(529))
        .mount(&server)
        .await;
    let c = client(&server, fast_retries(1));
    let q = one_noul();
    let request = Request {
        state: &"s",
        model: "jev-latest",
        questions: &q,
    };
    let once = CallOptions::new().retry(RetryPolicy::none());
    let err = c.evaluate_with(&request, &once).await.unwrap_err();
    assert!(
        matches!(err, Error::Overloaded { attempts: 1, .. }),
        "{err:?}"
    );
    let err = c.evaluate(&request).await.unwrap_err();
    assert!(
        matches!(err, Error::Overloaded { attempts: 2, .. }),
        "{err:?}"
    );
    assert_eq!(server.received_requests().await.unwrap().len(), 3);
    // The client's policy is unchanged by the call's.
    assert_eq!(c.retry().max_retries, 1);
}

#[tokio::test]
async fn an_unknown_kind_extra_fields_and_no_usage_are_tolerated() {
    // A response from a server newer than this crate, or a compatible one
    // that adds its own fields: an answer of a kind the crate does not know
    // (under an id nobody asked, so it is only kept), a top-level field the
    // API does not document, and no `usage` at all. Each used to fail the
    // whole response with a decode error.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header(REQUEST_ID_HEADER, "req_tolerant")
                .set_body_json(json!({
                    "model": "jev-2.0.0",
                    "answers": {
                        "x": { "type": "noul", "noul": 0.75 },
                        "later": { "type": "rank", "ranking": ["b", "a"] }
                    },
                    "routing": { "model": "typed-decisions" }
                })),
        )
        .expect(1)
        .mount(&server)
        .await;
    let mut q = Questions::new();
    let x = q.noul("x", "?", None).unwrap();

    let response = client(&server, RetryPolicy::none())
        .system_one(&"s", &q)
        .await
        .unwrap();
    assert!(response.get(&x).unwrap().is_yes(0.7));
    let later = &response.answers["later"];
    assert!(matches!(later, Answer::Unknown(_)), "{later:?}");
    assert_eq!(later.kind(), "rank");
    assert_eq!(response.usage, Usage::default());
    assert_eq!(
        response.extra.get("routing"),
        Some(&json!({ "model": "typed-decisions" }))
    );
    assert_eq!(response.extra.len(), 1, "{:?}", response.extra);
    assert_eq!(response.request_id.as_deref(), Some("req_tolerant"));
}

/// A choice over [`Department`] and a four-level Score, and the body of a
/// response that answers both as asked.
fn choice_and_score() -> (Questions, serde_json::Value) {
    let mut q = Questions::new();
    q.choice::<Department>("dept", "Which team handles `message`?")
        .unwrap();
    q.score(
        "impact",
        "How bad is `message`?",
        ["none", "minor", "major", "outage"],
    )
    .unwrap();
    let body = json!({
        "model": "jev-1.13.0",
        "answers": {
            "dept": { "type": "choice", "choice": "billing",
                      "probabilities": { "billing": 0.9, "technical": 0.1 }, "confidence": 0.8 },
            "impact": { "type": "score", "score": 1.0,
                        "legend": { "0": "none", "1": "minor", "2": "major", "3": "outage" },
                        "probabilities": { "0": 0.1, "1": 0.8, "2": 0.1, "3": 0.0 },
                        "confidence": 0.7 }
        },
        "usage": { "input_tokens": 40, "output_tokens": 4 }
    });
    (q, body)
}

#[tokio::test]
async fn the_client_refuses_a_choice_option_it_did_not_send() {
    let (q, mut body) = choice_and_score();
    body["answers"]["dept"] = json!({ "type": "choice", "choice": "sales",
                                      "probabilities": { "billing": 0.2, "sales": 0.8 },
                                      "confidence": 0.7 });
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header(REQUEST_ID_HEADER, "req_sales")
                .set_body_json(body),
        )
        // Once: an answer that does not fit is not retried.
        .expect(1)
        .mount(&server)
        .await;
    let err = client(&server, fast_retries(3))
        .system_one(&"s", &q)
        .await
        .unwrap_err();
    assert!(
        matches!(&err, Error::UnknownOption { id, option, request_id: Some(rid) }
            if id == "dept" && option == "sales" && rid == "req_sales"),
        "{err:?}"
    );
    assert!(err.is_unfit());
    assert_eq!(
        err.to_string(),
        r#"answer "dept" names option "sales", which its question does not offer [request_id req_sales]"#
    );
}

#[tokio::test]
async fn the_client_refuses_a_score_legend_that_is_not_the_levels_sent() {
    let (q, mut body) = choice_and_score();
    body["answers"]["impact"]["legend"] = json!({ "0": "a", "1": "b", "2": "c", "3": "d" });
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .expect(1)
        .mount(&server)
        .await;
    let err = client(&server, fast_retries(3))
        .system_one(&"s", &q)
        .await
        .unwrap_err();
    assert!(
        matches!(&err, Error::InvalidAnswer { id, reason, request_id: None }
            if id == "impact" && reason == "legend level 0 is not the level the question sent"),
        "{err:?}"
    );
}

/// Counts what a client's own observer is told.
#[derive(Default)]
struct CountingObserver {
    usage: AtomicUsize,
    input_tokens: AtomicUsize,
}

impl Observer for CountingObserver {
    fn on_usage(&self, _model: &str, usage: &Usage) {
        self.usage.fetch_add(1, Ordering::SeqCst);
        self.input_tokens.fetch_add(
            usize::try_from(usage.input_tokens).unwrap(),
            Ordering::SeqCst,
        );
    }
}

#[tokio::test]
async fn usage_is_reported_for_a_response_that_does_not_fit() {
    let (q, mut body) = choice_and_score();
    body["answers"].as_object_mut().unwrap().remove("impact");
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .expect(1)
        .mount(&server)
        .await;
    let observer = Arc::new(CountingObserver::default());
    let c = Client::builder()
        .api_key("test-key")
        .base_url(server.uri())
        .retry(fast_retries(2))
        .observer(observer.clone())
        .build()
        .unwrap();
    let err = c.system_one(&"s", &q).await.unwrap_err();
    assert!(
        matches!(&err, Error::MissingAnswer { id, .. } if id == "impact"),
        "{err:?}"
    );
    // The tokens were spent, so they are reported, once.
    assert_eq!(observer.usage.load(Ordering::SeqCst), 1);
    assert_eq!(observer.input_tokens.load(Ordering::SeqCst), 40);
}
