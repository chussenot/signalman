//! `GET /readyz` against mock upstreams: 200 with one entry per configured
//! upstream when all answer, 503 naming the one that does not, the deadline
//! turning a slow upstream into a named failure, and the cache reusing a
//! report for its lifetime.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use signalman::backstage::Enricher;
use signalman::readiness::{Probe, Settings};
use signalman::serve::{AppState, router};
use tower::ServiceExt;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// TypeSafe answering `GET /v1/models`, after `delay` when given.
async fn typesafe(delay: Option<Duration>) -> MockServer {
    let srv = MockServer::start().await;
    let mut response = ResponseTemplate::new(200).set_body_json(json!({
        "models": [{ "name": "jev-latest", "release_date": "2026-01-01", "description": "d" }]
    }));
    if let Some(d) = delay {
        response = response.set_delay(d);
    }
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(response)
        .mount(&srv)
        .await;
    srv
}

/// incident.io answering `GET /v1/identity` with `status`.
async fn incidentio(status: u16) -> MockServer {
    let srv = MockServer::start().await;
    let response = if status == 200 {
        ResponseTemplate::new(200).set_body_json(json!({
            "identity": { "name": "signalman key", "roles": ["viewer"], "dashboard_url": null }
        }))
    } else {
        ResponseTemplate::new(status).set_body_json(json!({
            "type": "internal_error", "status": status, "request_id": "r", "errors": []
        }))
    };
    Mock::given(method("GET"))
        .and(path("/v1/identity"))
        .respond_with(response)
        .mount(&srv)
        .await;
    srv
}

/// Backstage answering the one-component catalog query.
async fn catalog() -> MockServer {
    let srv = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/catalog/entities/by-query"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "items": [] })))
        .mount(&srv)
        .await;
    srv
}

fn app(
    ts: &MockServer,
    io: &MockServer,
    bs: Option<&MockServer>,
    settings: Settings,
) -> axum::Router {
    let mut triager = common::triager(ts, io);
    if let Some(bs) = bs {
        triager.backstage = Some(Enricher::new(common::backstage_client(&bs.uri())));
    }
    let mut state = AppState::new(None, triager);
    state.readiness = Probe::new(&state.triager, settings);
    router(Arc::new(state))
}

async fn readyz(app: &axum::Router) -> (StatusCode, Value) {
    let res = app
        .clone()
        .oneshot(Request::get("/readyz").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = res.status();
    assert_eq!(
        res.headers().get("cache-control").unwrap(),
        "no-store",
        "a readiness answer must never be cached downstream"
    );
    let body = res.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&body).unwrap())
}

fn upstream<'a>(body: &'a Value, service: &str) -> &'a Value {
    body["upstreams"]
        .as_array()
        .unwrap()
        .iter()
        .find(|u| u["service"] == service)
        .unwrap_or_else(|| panic!("no {service} entry in {body}"))
}

#[tokio::test]
async fn ready_when_every_configured_upstream_answers() {
    let (ts, io, bs) = (typesafe(None).await, incidentio(200).await, catalog().await);
    let app = app(&ts, &io, Some(&bs), Settings::default());

    let (status, body) = readyz(&app).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["ready"], json!(true));
    assert_eq!(body["cached"], json!(false));
    assert_eq!(body["upstreams"].as_array().unwrap().len(), 3, "{body}");
    for service in ["typesafe", "incidentio", "backstage"] {
        let u = upstream(&body, service);
        assert_eq!(u["ok"], json!(true), "{u}");
        assert!(u.get("error").is_none(), "{u}");
        assert!(u["latency_ms"].is_u64(), "{u}");
    }
    assert!(body["checked_at"].as_str().unwrap().ends_with('Z'));
}

#[tokio::test]
async fn without_a_catalog_only_two_upstreams_are_checked() {
    let (ts, io) = (typesafe(None).await, incidentio(200).await);
    let app = app(&ts, &io, None, Settings::default());

    let (status, body) = readyz(&app).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["upstreams"].as_array().unwrap().len(), 2, "{body}");
}

#[tokio::test]
async fn a_failing_upstream_is_named_with_a_503() {
    let (ts, io) = (typesafe(None).await, incidentio(500).await);
    let app = app(&ts, &io, None, Settings::default());

    let (status, body) = readyz(&app).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(body["ready"], json!(false));
    let failed = upstream(&body, "incidentio");
    assert_eq!(failed["ok"], json!(false));
    assert!(
        failed["error"].as_str().unwrap().contains("500"),
        "the error names the status: {failed}"
    );
    assert_eq!(upstream(&body, "typesafe")["ok"], json!(true), "{body}");
}

#[tokio::test]
async fn a_slow_upstream_fails_on_its_own_deadline() {
    let (ts, io) = (
        typesafe(Some(Duration::from_secs(3))).await,
        incidentio(200).await,
    );
    let settings = Settings {
        cache: Duration::ZERO,
        timeout: Duration::from_millis(300),
    };
    let app = app(&ts, &io, None, settings);

    let started = std::time::Instant::now();
    let (status, body) = readyz(&app).await;
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "the deadline, not the upstream, bounds the check"
    );
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    let slow = upstream(&body, "typesafe");
    assert!(
        slow["error"].as_str().unwrap().contains("timed out"),
        "{slow}"
    );
    assert_eq!(upstream(&body, "incidentio")["ok"], json!(true), "{body}");
}

/// The first answer is reused for the cache lifetime, failures included,
/// and a zero lifetime checks every time.
#[tokio::test]
async fn the_report_is_cached_for_its_lifetime() {
    let ts = typesafe(None).await;
    let io = MockServer::start().await;
    // One good answer, then the key is revoked.
    Mock::given(method("GET"))
        .and(path("/v1/identity"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "identity": { "name": "k", "roles": [] }
        })))
        .up_to_n_times(1)
        .mount(&io)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/identity"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "type": "authentication_error", "status": 401, "request_id": "r", "errors": []
        })))
        .mount(&io)
        .await;

    let cached = app(&ts, &io, None, Settings::default());
    let (status, body) = readyz(&cached).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = readyz(&cached).await;
    assert_eq!(status, StatusCode::OK, "still the cached report: {body}");
    assert_eq!(body["cached"], json!(true));

    let uncached = app(
        &ts,
        &io,
        None,
        Settings {
            cache: Duration::ZERO,
            timeout: Duration::from_secs(3),
        },
    );
    let (status, body) = readyz(&uncached).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(body["cached"], json!(false));
    assert!(
        upstream(&body, "incidentio")["error"]
            .as_str()
            .unwrap()
            .contains("401"),
        "a revoked key shows up as not ready: {body}"
    );
}
