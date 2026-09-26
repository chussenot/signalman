//! What the TypeSafe client reports to the process-wide observer for a 2xx
//! it cannot use: `decode` for a body that does not decode (on both
//! endpoints), `unfit` for a response that does not answer the questions
//! it was sent, and `too_large` for a body over the cap; never a status
//! code, and never retried.
//!
//! A target of its own, with one test: `observer::set_global` succeeds once
//! per process, and any other test in the same binary would add its own
//! failed attempts to the counts.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use judgment::client::REQUEST_ID_HEADER;
use judgment::{Client, Error, Observer, Questions, RetryPolicy, observer};
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Every failed attempt the global observer is told about, in order.
#[derive(Default)]
struct Failures(Mutex<Vec<(&'static str, String)>>);

impl Observer for Failures {
    fn on_failed_attempt(&self, service: &'static str, status: &str) {
        self.0.lock().unwrap().push((service, status.to_owned()));
    }
}

#[tokio::test]
async fn an_unfit_or_undecodable_200_is_counted_as_a_failed_attempt() {
    let failures = Arc::new(Failures::default());
    assert!(
        observer::set_global(failures.clone()),
        "set once per process"
    );

    let server = MockServer::start().await;
    let c = Client::builder()
        .api_key("test-key")
        .base_url(server.uri())
        .retry(RetryPolicy {
            max_retries: 3,
            backoff_initial: Duration::from_millis(5),
            backoff_max: Duration::from_millis(20),
            max_body_bytes: 256,
            ..RetryPolicy::default()
        })
        .build()
        .unwrap();
    let mut q = Questions::new();
    q.noul("x", "Is `message` urgent?", None).unwrap();

    // A 200 whose body is not the documented shape.
    Mock::given(method("POST"))
        .and(path("/v1/systemone"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<html>not the API</html>"))
        .expect(1)
        .mount(&server)
        .await;
    let err = c.system_one(&"s", &q).await.unwrap_err();
    assert!(matches!(err, Error::Decode { .. }), "{err:?}");
    server.verify().await;
    server.reset().await;

    // A 200 that decodes and answers another question than the one sent.
    Mock::given(method("POST"))
        .and(path("/v1/systemone"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header(REQUEST_ID_HEADER, "req_unfit")
                .set_body_json(json!({
                    "model": "jev-1.13.0",
                    "answers": { "y": { "type": "noul", "noul": 0.5 } },
                    "usage": { "input_tokens": 3, "output_tokens": 1 }
                })),
        )
        .expect(1)
        .mount(&server)
        .await;
    let err = c.system_one(&"s", &q).await.unwrap_err();
    assert!(
        matches!(&err, Error::MissingAnswer { id, .. } if id == "x"),
        "{err:?}"
    );
    assert_eq!(err.request_id(), Some("req_unfit"));
    server.verify().await;
    server.reset().await;

    // A 200 from the models list that is not a list of models.
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "data": [] })))
        .expect(1)
        .mount(&server)
        .await;
    let err = c.list_models().await.unwrap_err();
    assert!(matches!(err, Error::Decode { .. }), "{err:?}");
    server.verify().await;
    server.reset().await;

    // A 200 whose body is over the cap: refused before it is read.
    Mock::given(method("POST"))
        .and(path("/v1/systemone"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![b'{'; 257]))
        .expect(1)
        .mount(&server)
        .await;
    let err = c.system_one(&"s", &q).await.unwrap_err();
    assert!(
        matches!(err, Error::ResponseTooLarge { limit: 256 }),
        "{err:?}"
    );
    server.verify().await;

    let seen = failures.0.lock().unwrap().clone();
    assert_eq!(
        seen,
        [
            ("typesafe", "decode".to_owned()),
            ("typesafe", "unfit".to_owned()),
            ("typesafe", "decode".to_owned()),
            ("typesafe", "too_large".to_owned()),
        ],
        "one failed attempt each, none of them an HTTP status"
    );
    assert!(
        seen.iter()
            .all(|(_, status)| status.parse::<u16>().is_err()),
        "{seen:?}"
    );
}
