//! What the client writes on its spans, seen through a capturing
//! `tracing_subscriber` layer: the `request_id` field of `typesafe.evaluate`
//! and `typesafe.list_models` on success and on failure, its absence when
//! no id came back, the `debug` event that drops an overlong id without
//! logging it, the one `typesafe.evaluate` span of `evaluate_with`, which
//! carries no per-call option, and the `warn` event for an answer of a kind
//! the client does not know, with its key and kind escaped and cut.
//!
//! Its own test target (see `Cargo.toml`): tracing caches each callsite's
//! interest process-wide, and a binary of its own keeps the other client
//! tests' subscribers out of that cache. Each test installs its subscriber
//! with `set_default` on a current-thread runtime, before any client call, so
//! everything the client does runs on the thread the subscriber covers.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::too_many_lines)]

use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use judgment::client::{HeaderName, HeaderValue, REQUEST_ID_HEADER};
use judgment::{CallOptions, Client, Questions, Request, RetryPolicy};
use serde_json::json;
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::layer::{Context, Layer, SubscriberExt};
use tracing_subscriber::registry::{LookupSpan, Registry};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// One thing the layer saw.
#[derive(Debug, Clone, PartialEq)]
enum Captured {
    /// A span field given a value, at creation or by `record`.
    Field {
        span: String,
        field: String,
        value: String,
    },
    /// An event and its fields.
    Event {
        level: Level,
        fields: Vec<(String, String)>,
    },
}

/// A layer that keeps everything it sees.
#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<Captured>>>);

impl Capture {
    /// Everything captured so far, emptying the buffer.
    fn take(&self) -> Vec<Captured> {
        std::mem::take(&mut *self.0.lock().unwrap())
    }

    fn push(&self, captured: Captured) {
        self.0.lock().unwrap().push(captured);
    }

    fn push_fields(&self, span: &str, fields: Fields) {
        for (field, value) in fields.0 {
            self.push(Captured::Field {
                span: span.to_owned(),
                field,
                value,
            });
        }
    }
}

/// The fields a span or event carries, as strings. A `field::Empty` field
/// is not visited, so it never appears here.
#[derive(Default)]
struct Fields(Vec<(String, String)>);

impl Visit for Fields {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.0.push((field.name().to_owned(), value.to_owned()));
    }

    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        self.0.push((field.name().to_owned(), format!("{value:?}")));
    }
}

impl<S: Subscriber + for<'a> LookupSpan<'a>> Layer<S> for Capture {
    fn on_new_span(&self, attrs: &Attributes<'_>, _id: &Id, _ctx: Context<'_, S>) {
        let mut fields = Fields::default();
        attrs.record(&mut fields);
        self.push_fields(attrs.metadata().name(), fields);
    }

    fn on_record(&self, id: &Id, values: &Record<'_>, ctx: Context<'_, S>) {
        let span = ctx
            .span(id)
            .map_or_else(|| "?".to_owned(), |s| s.name().to_owned());
        let mut fields = Fields::default();
        values.record(&mut fields);
        self.push_fields(&span, fields);
    }

    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let mut fields = Fields::default();
        event.record(&mut fields);
        self.push(Captured::Event {
            level: *event.metadata().level(),
            fields: fields.0,
        });
    }
}

/// The values `span` was given for `field`, in order.
fn values(captured: &[Captured], span: &str, field: &str) -> Vec<String> {
    captured
        .iter()
        .filter_map(|c| match c {
            Captured::Field {
                span: s,
                field: f,
                value,
            } if s == span && f == field => Some(value.clone()),
            _ => None,
        })
        .collect()
}

fn client(base_url: &str) -> Client {
    Client::builder()
        .api_key("test-key")
        .base_url(base_url)
        .retry(RetryPolicy::none())
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap()
}

fn one_noul() -> Questions {
    let mut q = Questions::new();
    q.noul("x", "?", None).unwrap();
    q
}

fn ok(request_id: Option<&str>) -> ResponseTemplate {
    let t = ResponseTemplate::new(200).set_body_json(json!({
        "model": "jev-1.13.0",
        "answers": { "x": { "type": "noul", "noul": 0.5 } },
        "usage": { "input_tokens": 3, "output_tokens": 1 }
    }));
    match request_id {
        Some(id) => t.insert_header(REQUEST_ID_HEADER, id),
        None => t,
    }
}

async fn serve_once(server: &MockServer, http_method: &str, template: ResponseTemplate) {
    server.reset().await;
    Mock::given(method(http_method))
        .respond_with(template)
        .expect(1)
        .mount(server)
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn the_span_records_the_request_id_on_success_and_on_failure() {
    let cap = Capture::default();
    let _g = tracing::subscriber::set_default(Registry::default().with(cap.clone()));
    let server = MockServer::start().await;
    let c = client(&server.uri());
    let q = one_noul();

    // Success: the response's header.
    serve_once(&server, "POST", ok(Some("req_ok"))).await;
    c.system_one(&"s", &q).await.unwrap();
    let seen = cap.take();
    assert_eq!(
        values(&seen, "typesafe.evaluate", "request_id"),
        ["req_ok"],
        "{seen:#?}"
    );
    assert_eq!(values(&seen, "typesafe.evaluate", "input_tokens"), ["3"]);

    // An HTTP error: the error response's header.
    serve_once(
        &server,
        "POST",
        ResponseTemplate::new(401).insert_header(REQUEST_ID_HEADER, "req_401"),
    )
    .await;
    c.system_one(&"s", &q).await.unwrap_err();
    let seen = cap.take();
    assert_eq!(
        values(&seen, "typesafe.evaluate", "request_id"),
        ["req_401"],
        "{seen:#?}"
    );

    // A 2xx that does not decode is still a response with an id.
    serve_once(
        &server,
        "POST",
        ResponseTemplate::new(200)
            .insert_header(REQUEST_ID_HEADER, "req_html")
            .set_body_string("<html>"),
    )
    .await;
    c.system_one(&"s", &q).await.unwrap_err();
    let seen = cap.take();
    assert_eq!(
        values(&seen, "typesafe.evaluate", "request_id"),
        ["req_html"],
        "{seen:#?}"
    );

    // The models list records its id on its own span.
    server.reset().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header(REQUEST_ID_HEADER, "req_models")
                .set_body_json(json!({ "models": [] })),
        )
        .expect(1)
        .mount(&server)
        .await;
    c.list_models().await.unwrap();
    let seen = cap.take();
    assert_eq!(
        values(&seen, "typesafe.list_models", "request_id"),
        ["req_models"],
        "{seen:#?}"
    );

    // No header: the span exists, the field stays absent.
    serve_once(&server, "POST", ok(None)).await;
    c.system_one(&"s", &q).await.unwrap();
    let seen = cap.take();
    assert!(
        !values(&seen, "typesafe.evaluate", "model").is_empty(),
        "the span was captured: {seen:#?}"
    );
    assert!(
        values(&seen, "typesafe.evaluate", "request_id").is_empty(),
        "{seen:#?}"
    );

    // An id over 256 bytes is dropped, and the debug event says how long it
    // was without repeating it.
    let long = "r".repeat(300);
    serve_once(&server, "POST", ok(Some(&long))).await;
    let response = c.system_one(&"s", &q).await.unwrap();
    assert_eq!(response.request_id, None);
    let seen = cap.take();
    assert!(
        values(&seen, "typesafe.evaluate", "request_id").is_empty(),
        "{seen:#?}"
    );
    let dropped: Vec<&Vec<(String, String)>> = seen
        .iter()
        .filter_map(|c| match c {
            Captured::Event { level, fields }
                if *level == Level::DEBUG
                    && fields
                        .iter()
                        .any(|(k, v)| k == "message" && v.contains("too long")) =>
            {
                Some(fields)
            }
            _ => None,
        })
        .collect();
    assert_eq!(dropped.len(), 1, "{seen:#?}");
    assert!(
        dropped[0].iter().any(|(k, v)| k == "len" && v == "300"),
        "{dropped:?}"
    );
    assert!(
        seen.iter().all(|c| !format!("{c:?}").contains(&long)),
        "the overlong id reached a span or event"
    );

    // A transport failure: no response, no id.
    let refused = client("http://127.0.0.1:1");
    refused.system_one(&"s", &q).await.unwrap_err();
    let seen = cap.take();
    assert!(
        !values(&seen, "typesafe.evaluate", "model").is_empty(),
        "the span was captured: {seen:#?}"
    );
    assert!(
        values(&seen, "typesafe.evaluate", "request_id").is_empty(),
        "{seen:#?}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn evaluate_with_records_the_request_id_on_its_one_span() {
    let cap = Capture::default();
    let _g = tracing::subscriber::set_default(Registry::default().with(cap.clone()));
    let server = MockServer::start().await;
    let c = client(&server.uri());
    let q = one_noul();
    let request = Request {
        state: &"s",
        model: "jev-latest",
        questions: &q,
    };
    let mut token = HeaderValue::from_static("SECRET-HEADER");
    token.set_sensitive(true);
    let options = CallOptions::new()
        .timeout(Duration::from_secs(2))
        .header(HeaderName::from_static("x-gateway-token"), token)
        .unwrap()
        .extra("beam_width", "SECRET-EXTRA")
        .unwrap();

    serve_once(&server, "POST", ok(Some("req_with"))).await;
    c.evaluate_with(&request, &options).await.unwrap();
    let seen = cap.take();
    assert_eq!(
        values(&seen, "typesafe.evaluate", "request_id"),
        ["req_with"],
        "{seen:#?}"
    );
    assert_eq!(values(&seen, "typesafe.evaluate", "model"), ["jev-latest"]);
    assert_eq!(values(&seen, "typesafe.evaluate", "input_tokens"), ["3"]);
    let shown = format!("{seen:?}");
    for secret in [
        "SECRET-HEADER",
        "SECRET-EXTRA",
        "x-gateway-token",
        "beam_width",
    ] {
        assert!(!shown.contains(secret), "{secret} reached a span: {shown}");
    }

    // On failure too.
    serve_once(
        &server,
        "POST",
        ResponseTemplate::new(529).insert_header(REQUEST_ID_HEADER, "req_529"),
    )
    .await;
    c.evaluate_with(&request, &options).await.unwrap_err();
    let seen = cap.take();
    assert_eq!(
        values(&seen, "typesafe.evaluate", "request_id"),
        ["req_529"],
        "{seen:#?}"
    );

    // `evaluate` goes through `evaluate_with`: still one span per call.
    serve_once(&server, "POST", ok(Some("req_plain"))).await;
    c.evaluate(&request).await.unwrap();
    let seen = cap.take();
    assert_eq!(
        values(&seen, "typesafe.evaluate", "model"),
        ["jev-latest"],
        "exactly one typesafe.evaluate span: {seen:#?}"
    );
    assert_eq!(
        values(&seen, "typesafe.evaluate", "request_id"),
        ["req_plain"]
    );
}

/// The warn events in `captured`, each as its fields.
fn warnings(captured: &[Captured]) -> Vec<&Vec<(String, String)>> {
    captured
        .iter()
        .filter_map(|c| match c {
            Captured::Event { level, fields } if *level == Level::WARN => Some(fields),
            _ => None,
        })
        .collect()
}

/// The value of `name` among an event's fields.
fn field<'a>(fields: &'a [(String, String)], name: &str) -> Option<&'a str> {
    fields
        .iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.as_str())
}

fn with_answers(
    answers: &serde_json::Value,
    extra: &[(&str, serde_json::Value)],
) -> ResponseTemplate {
    let mut body = json!({
        "model": "jev-1.13.0",
        "answers": answers,
        "usage": { "input_tokens": 3, "output_tokens": 1 }
    });
    for (key, value) in extra {
        body[*key] = value.clone();
    }
    ResponseTemplate::new(200).set_body_json(body)
}

#[tokio::test(flavor = "current_thread")]
async fn an_unknown_answer_is_warned_once_with_question_and_kind() {
    let cap = Capture::default();
    let _g = tracing::subscriber::set_default(Registry::default().with(cap.clone()));
    let server = MockServer::start().await;
    let c = client(&server.uri());
    let q = one_noul();
    let noul = json!({ "type": "noul", "noul": 0.5 });

    // One unknown answer beside a known one: one warning, naming the
    // question and the kind, with no other field of its own.
    serve_once(
        &server,
        "POST",
        with_answers(
            &json!({ "x": noul, "later": { "type": "rank", "ranking": [] } }),
            &[],
        ),
    )
    .await;
    c.system_one(&"s", &q).await.unwrap();
    let seen = cap.take();
    let warned = warnings(&seen);
    assert_eq!(warned.len(), 1, "{seen:#?}");
    let mut names: Vec<&str> = warned[0].iter().map(|(k, _)| k.as_str()).collect();
    names.sort_unstable();
    assert_eq!(names, ["kind", "message", "question"], "{warned:?}");
    assert_eq!(field(warned[0], "question"), Some("later"));
    assert_eq!(field(warned[0], "kind"), Some("rank"));
    assert!(
        field(warned[0], "message").is_some_and(|m| m.contains("Answer::Unknown")),
        "{warned:?}"
    );

    // A long kind is cut to 64 characters in the event, and a line break in
    // it is escaped.
    let long = format!("a\nb{}", "k".repeat(100));
    serve_once(
        &server,
        "POST",
        with_answers(&json!({ "x": noul, "later": { "type": long } }), &[]),
    )
    .await;
    c.system_one(&"s", &q).await.unwrap();
    let seen = cap.take();
    let warned = warnings(&seen);
    assert_eq!(warned.len(), 1, "{seen:#?}");
    let kind = field(warned[0], "kind").unwrap();
    assert_eq!(kind.chars().count(), 64, "{kind:?}");
    assert!(kind.starts_with(r"a\nb"), "{kind:?}");
    assert!(!kind.contains('\n'), "{kind:?}");
    assert!(
        seen.iter()
            .all(|c| !format!("{c:?}").contains(&"k".repeat(100))),
        "the whole kind reached a span or event"
    );

    // The key of an answer to a question that was not asked is the server's
    // own string too, and `verify` keeps it (it walks the asked questions
    // only), so it is escaped and cut like the kind.
    let key = format!("q\nq{}", "i".repeat(100));
    serve_once(
        &server,
        "POST",
        with_answers(&json!({ "x": noul, key: { "type": "rank" } }), &[]),
    )
    .await;
    c.system_one(&"s", &q).await.unwrap();
    let seen = cap.take();
    let warned = warnings(&seen);
    assert_eq!(warned.len(), 1, "{seen:#?}");
    let question = field(warned[0], "question").unwrap();
    assert_eq!(question.chars().count(), 64, "{question:?}");
    assert!(question.starts_with(r"q\nq"), "{question:?}");
    assert!(!question.contains('\n'), "{question:?}");
    assert_eq!(field(warned[0], "kind"), Some("rank"));
    assert!(
        seen.iter()
            .all(|c| !format!("{c:?}").contains(&"i".repeat(100))),
        "the whole key reached a span or event"
    );

    // Undocumented top-level fields are expected from a compatible server:
    // kept, never warned about.
    serve_once(
        &server,
        "POST",
        with_answers(
            &json!({ "x": noul }),
            &[
                ("id", json!("gen-dec-0001")),
                ("provider", json!("TypeSafe")),
                ("routing", json!({ "model": "typed-decisions" })),
            ],
        ),
    )
    .await;
    let response = c.system_one(&"s", &q).await.unwrap();
    assert_eq!(response.extra.len(), 3);
    let seen = cap.take();
    assert!(warnings(&seen).is_empty(), "{seen:#?}");
}
