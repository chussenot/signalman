//! OpenTelemetry export end to end: a `wiremock` server stands in for an
//! OTLP/HTTP collector, a triage runs against mock TypeSafe and incident.io,
//! and after shutdown the collector has received protobuf `POST /v1/traces`
//! and `POST /v1/metrics` bodies naming signalman's spans, metrics, and the
//! attribute values the triage produced. Protobuf keeps strings verbatim, so
//! the bodies can be searched without decoding them.
//!
//! One test per process: `Providers::init` installs the global subscriber,
//! which can only happen once. That is also asserted here.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::too_many_lines)]

use std::time::Duration;

use serde_json::json;
use signalman::incidentio::{Triager, WriteBack};
use signalman::telemetry::{Error, Providers, Settings};
use signalman::{Client, RetryPolicy};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// The fake collector: accepts every OTLP signal.
async fn collector() -> MockServer {
    let srv = MockServer::start().await;
    for p in ["/v1/traces", "/v1/metrics"] {
        Mock::given(method("POST"))
            .and(path(p))
            .respond_with(ResponseTemplate::new(200))
            .mount(&srv)
            .await;
    }
    srv
}

/// incident.io with alert `al-1` and one open incident, TypeSafe paging the
/// `application` team; every write endpoint expected zero times (dry run).
async fn upstreams() -> (MockServer, MockServer) {
    let incidentio = MockServer::start().await;
    let typesafe = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v2/alerts/al-1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "alert": {
                "id": "al-1", "alert_source_id": "src-dd", "title": "HighErrorRate checkout-api",
                "description": "5xx ratio 12% for 10m", "status": "firing",
                "created_at": "2026-09-20T11:58:00Z", "attributes": [], "tags": []
            }
        })))
        .mount(&incidentio)
        .await;
    Mock::given(method("GET"))
        .and(path("/v2/alerts/al-missing"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({
            "type": "not_found", "status": 404, "request_id": "r", "errors": []
        })))
        .mount(&incidentio)
        .await;
    Mock::given(method("GET"))
        .and(path("/v2/incidents"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "incidents": [{
                "id": "01INC4821", "reference": "INC-4821", "name": "Checkout 5xx spike",
                "permalink": "https://app.incident.io/org/incidents/4821",
                "incident_status": { "id": "s", "name": "Active", "category": "live" },
                "mode": "standard"
            }],
            "pagination_meta": { "page_size": 40 }
        })))
        .mount(&incidentio)
        .await;
    Mock::given(method("GET"))
        .and(path("/v2/alerts"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "alerts": [], "pagination_meta": { "page_size": 50 }
        })))
        .mount(&incidentio)
        .await;
    for p in [
        "/v2/alerts/al-1/actions/add_tags",
        "/v2/incident_alerts",
        "/v1/alert_notes",
    ] {
        Mock::given(method("POST"))
            .and(path(p))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
            .expect(0)
            .mount(&incidentio)
            .await;
    }
    Mock::given(method("POST"))
        .and(path("/v1/systemone"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "model": "jev-1.13.0",
            "answers": {
                "owner": { "type": "choice", "choice": "application",
                           "probabilities": { "application": 0.8, "platform": 0.2 }, "confidence": 0.75 },
                "impact": { "type": "score", "score": 2.0, "legend": { "0": "a", "1": "b", "2": "c", "3": "d" },
                            "probabilities": { "0": 0.0, "1": 0.0, "2": 1.0, "3": 0.0 }, "confidence": 0.9 },
                "actionable": { "type": "noul", "noul": 0.95 },
                "duplicate_of": { "type": "choice", "choice": "none",
                                  "probabilities": { "INC-4821": 0.1, "none": 0.9 }, "confidence": 0.8 },
                "caused_by_change": { "type": "noul", "noul": 0.2 }
            },
            "usage": { "input_tokens": 500, "output_tokens": 30 }
        })))
        .mount(&typesafe)
        .await;
    (incidentio, typesafe)
}

/// The bodies of every request the collector received on `signal_path`.
async fn received(collector: &MockServer, signal_path: &str) -> String {
    collector
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|r| r.url.path() == signal_path)
        .map(|r| String::from_utf8_lossy(&r.body).into_owned())
        .collect::<Vec<_>>()
        .join("\n")
}

// The exporter sends from the SDK's own thread with a blocking client and
// `shutdown` waits for it, so the runtime must keep serving the collector
// meanwhile: two workers, and the shutdown off the runtime.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_triage_exports_its_spans_and_metrics_to_the_collector() {
    let collector = collector().await;
    let (incidentio, typesafe) = upstreams().await;

    let settings = Settings {
        otlp_endpoint: Some(format!("{}/", collector.uri())),
        service_name: "signalman-under-test".to_owned(),
        // Longer than the test: everything arrives on shutdown's flush.
        metrics_interval: Duration::from_secs(3600),
    };
    let providers = Providers::init(&settings).expect("first init succeeds");
    assert!(
        matches!(
            Providers::init(&settings),
            Err(Error::AlreadyInitialised(_))
        ),
        "a second init must be refused, not silently ignored"
    );

    let ts = Client::builder()
        .api_key("ts")
        .base_url(typesafe.uri())
        .retry(RetryPolicy::none())
        .build()
        .unwrap();
    let io = signalman::incidentio::Client::builder()
        .api_key("io")
        .base_url(incidentio.uri())
        .retry(RetryPolicy::none())
        .build()
        .unwrap();
    let mut triager = Triager::new(ts, io);
    triager.write_back = WriteBack::DryRun;

    let outcome = triager.triage_alert_by_id("al-1").await.unwrap();
    assert_eq!(outcome.decision.key(), "page");
    // One upstream failure, so the error counter has something to count.
    assert!(triager.incidentio.get_alert("al-missing").await.is_err());

    tokio::task::spawn_blocking(move || providers.shutdown())
        .await
        .unwrap();

    let traces = received(&collector, "/v1/traces").await;
    assert!(!traces.is_empty(), "no trace export reached the collector");
    for span in [
        "triage",
        "triage.flow",
        "incidentio.get_alert",
        "incidentio.list_incidents",
        "incidentio.list_firing_alerts",
        "typesafe.evaluate",
    ] {
        assert!(
            traces.contains(span),
            "span {span:?} missing from the export"
        );
    }
    assert!(
        traces.contains("signalman-under-test"),
        "service.name resource attribute missing"
    );
    assert!(
        !traces.contains("incidentio.write_back"),
        "a dry run must not open the write-back span"
    );

    let metrics = received(&collector, "/v1/metrics").await;
    assert!(
        !metrics.is_empty(),
        "no metrics export reached the collector"
    );
    for name in [
        "signalman.triage.count",
        "signalman.triage.duration",
        "signalman.alert.time_to_qualify",
        "signalman.typesafe.tokens",
        "signalman.upstream.errors",
    ] {
        assert!(
            metrics.contains(name),
            "metric {name:?} missing from the export"
        );
    }
    for value in [
        "page",
        "application",
        "dry_run",
        "jev-1.13.0",
        "incidentio",
        "404",
    ] {
        assert!(
            metrics.contains(value),
            "attribute value {value:?} missing from the metrics export"
        );
    }
}
