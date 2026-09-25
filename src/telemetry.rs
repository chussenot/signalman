//! OpenTelemetry for the receiver and the CLI: spans over the triage flow
//! and metrics for the product and its upstreams, exported over OTLP/HTTP
//! when `telemetry.otlp_endpoint` (`OTEL_EXPORTER_OTLP_ENDPOINT`) is set.
//! Without an endpoint nothing is exported and nothing changes: the
//! `tracing` text log on stderr, filtered by `RUST_LOG`, is installed either
//! way, and every span and counter below is a no-op.
//!
//! This module is the one place that names an instrument, so the table in
//! `docs/observability.md` has a single source. Spans come from
//! `#[tracing::instrument]` on the client methods and the flow
//! (`typesafe.evaluate`, `incidentio.*`, `backstage.*`, `triage`,
//! `triage.flow`, `incidentio.write_back`, `webhook.receive`) and reach the
//! exporter through `tracing-opentelemetry`. Metrics use the OpenTelemetry
//! API directly, through [`metrics`], created once from the global meter.
//!
//! Export is OTLP/HTTP with protobuf bodies (`/v1/traces`, `/v1/metrics`
//! appended to the configured base, as the specification does for the
//! generic endpoint variable), sent by a blocking `reqwest` client on the
//! SDK's own thread: no gRPC stack, and no dependency on the tokio runtime
//! being alive at flush time. `OTEL_EXPORTER_OTLP_HEADERS` (for example a
//! collector's bearer token) is read by the exporter itself, like every
//! other secret: never from the configuration file.

use std::sync::{OnceLock, Weak};
use std::time::Duration;

use opentelemetry::metrics::{Counter, Histogram};
use opentelemetry::trace::TracerProvider as _;
use opentelemetry::{KeyValue, global};
use opentelemetry_otlp::{MetricExporter, Protocol, SpanExporter, WithExportConfig};
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::metrics::{PeriodicReader, SdkMeterProvider, Temporality};
use opentelemetry_sdk::trace::SdkTracerProvider;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

use crate::serve::AppState;

/// Instrumentation scope name for the meter and the tracer.
pub const SCOPE: &str = "signalman";

/// `service.name` when nothing else is configured.
pub const DEFAULT_SERVICE_NAME: &str = "signalman";

/// How often metrics are exported when nothing else is configured.
pub const DEFAULT_METRICS_INTERVAL: Duration = Duration::from_secs(60);

/// What [`Providers::init`] needs, resolved by `config` like every other
/// setting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    /// OTLP/HTTP base endpoint, e.g. `http://otel-collector:4318`. `None`
    /// exports nothing.
    pub otlp_endpoint: Option<String>,
    /// `service.name` on every span and metric.
    pub service_name: String,
    /// Metrics export interval.
    pub metrics_interval: Duration,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            otlp_endpoint: None,
            service_name: DEFAULT_SERVICE_NAME.to_owned(),
            metrics_interval: DEFAULT_METRICS_INTERVAL,
        }
    }
}

/// Telemetry could not be set up. Both variants happen before any work
/// starts, so the process exits with the message.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// An OTLP exporter refused its configuration (an endpoint that is not
    /// a URI, most likely).
    #[error("cannot build the OTLP {signal} exporter for {endpoint}: {source}")]
    Exporter {
        /// `traces` or `metrics`.
        signal: &'static str,
        /// The configured base endpoint.
        endpoint: String,
        /// The SDK's reason.
        #[source]
        source: opentelemetry_otlp::ExporterBuildError,
    },
    /// A global `tracing` subscriber was already installed; `init` runs once
    /// per process.
    #[error("telemetry is already initialised: {0}")]
    AlreadyInitialised(#[source] tracing_subscriber::util::TryInitError),
}

/// The SDK providers behind the global subscriber and meter, kept so
/// [`Providers::shutdown`] can flush them. Without an endpoint both are
/// `None` and shutdown is a no-op.
#[derive(Debug)]
pub struct Providers {
    tracer: Option<SdkTracerProvider>,
    meter: Option<SdkMeterProvider>,
}

impl Providers {
    /// Install the global `tracing` subscriber (text log on stderr filtered
    /// by `RUST_LOG`, plus the OpenTelemetry layer when exporting) and the
    /// global meter provider. Call once, before any span or measurement.
    pub fn init(settings: &Settings) -> Result<Self, Error> {
        // Before the first client exists: a client without its own observer
        // reads the global one lazily, but installing first keeps the order
        // obvious. Idempotent, so a second `init` (refused below) is harmless.
        judgment::observer::set_global(std::sync::Arc::new(Observer));
        let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
        let text = tracing_subscriber::fmt::layer().with_writer(std::io::stderr);

        let Some(base) = settings
            .otlp_endpoint
            .as_deref()
            .map(|e| e.trim().trim_end_matches('/'))
            .filter(|e| !e.is_empty())
        else {
            tracing_subscriber::registry()
                .with(filter)
                .with(text)
                .try_init()
                .map_err(Error::AlreadyInitialised)?;
            return Ok(Self {
                tracer: None,
                meter: None,
            });
        };

        let resource = Resource::builder()
            .with_service_name(settings.service_name.clone())
            .with_attribute(KeyValue::new("service.version", env!("CARGO_PKG_VERSION")))
            .build();

        let spans = SpanExporter::builder()
            .with_http()
            .with_protocol(Protocol::HttpBinary)
            .with_endpoint(format!("{base}/v1/traces"))
            .build()
            .map_err(|source| Error::Exporter {
                signal: "traces",
                endpoint: base.to_owned(),
                source,
            })?;
        let tracer = SdkTracerProvider::builder()
            .with_resource(resource.clone())
            .with_batch_exporter(spans)
            .build();

        let measurements = MetricExporter::builder()
            .with_http()
            .with_protocol(Protocol::HttpBinary)
            .with_endpoint(format!("{base}/v1/metrics"))
            .with_temporality(Temporality::Cumulative)
            .build()
            .map_err(|source| Error::Exporter {
                signal: "metrics",
                endpoint: base.to_owned(),
                source,
            })?;
        let reader = PeriodicReader::builder(measurements)
            .with_interval(settings.metrics_interval)
            .build();
        let meter = SdkMeterProvider::builder()
            .with_resource(resource)
            .with_reader(reader)
            .build();

        let otel = tracing_opentelemetry::layer()
            .with_tracer(tracer.tracer(SCOPE))
            .with_error_records_to_exceptions(true);
        tracing_subscriber::registry()
            .with(filter)
            .with(text)
            .with(otel)
            .try_init()
            .map_err(Error::AlreadyInitialised)?;
        global::set_meter_provider(meter.clone());

        tracing::info!(
            endpoint = base,
            service_name = %settings.service_name,
            metrics_interval_seconds = settings.metrics_interval.as_secs(),
            "OpenTelemetry export enabled (OTLP/HTTP)"
        );
        Ok(Self {
            tracer: Some(tracer),
            meter: Some(meter),
        })
    }

    /// Flush and stop both providers. Blocks until the last batch is sent
    /// or the SDK gives up, so a one-shot CLI run still exports its spans.
    pub fn shutdown(self) {
        if let Some(meter) = self.meter
            && let Err(e) = meter.shutdown()
        {
            tracing::warn!(error = %e, "metrics export did not flush cleanly");
        }
        if let Some(tracer) = self.tracer
            && let Err(e) = tracer.shutdown()
        {
            tracing::warn!(error = %e, "trace export did not flush cleanly");
        }
    }
}

// ---------------------------------------------------------------------------
// Metrics
// ---------------------------------------------------------------------------

/// Every instrument signalman records. Created once from the global meter,
/// so `init` must run first in a process that exports; in one that does not
/// (tests, the CLI without an endpoint) every call is a no-op.
pub struct Metrics {
    /// `signalman.triage.count`: triages decided, by `decision`, `team` and
    /// `mode` (`applied` or `dry_run`).
    pub triages: Counter<u64>,
    /// `signalman.triage.duration`: seconds from the start of a triage to
    /// its decision and write-back, by `decision`.
    pub triage_duration: Histogram<f64>,
    /// `signalman.alert.time_to_qualify`: seconds from the alert's creation
    /// in incident.io to signalman's decision, by `decision`. The product
    /// metric; recorded only when the alert carried a creation time.
    pub time_to_qualify: Histogram<f64>,
    /// `signalman.typesafe.tokens`: tokens per TypeSafe response, by `model`
    /// and `direction` (`input`, which is billed, or `output`).
    pub typesafe_tokens: Counter<u64>,
    /// `signalman.upstream.errors`: failed attempts against an upstream, by
    /// `service` (`typesafe`, `incidentio`, `backstage`) and `status` (the
    /// HTTP status, `transport`, or for `typesafe` `decode` and `unfit`).
    /// Every attempt counts, retried or not.
    pub upstream_errors: Counter<u64>,
    /// `signalman.webhook.deliveries`: incident.io deliveries by `result`
    /// (`accepted`, `duplicate`, `ignored`, `rejected`, `invalid`,
    /// `refused`).
    pub webhook_deliveries: Counter<u64>,
}

impl Metrics {
    fn new() -> Self {
        let meter = global::meter(SCOPE);
        Self {
            triages: meter
                .u64_counter("signalman.triage.count")
                .with_description("Triages decided, by decision, team and write mode")
                .with_unit("{triage}")
                .build(),
            triage_duration: meter
                .f64_histogram("signalman.triage.duration")
                .with_description(
                    "Seconds from the start of a triage to its decision and write-back",
                )
                .with_unit("s")
                .build(),
            time_to_qualify: meter
                .f64_histogram("signalman.alert.time_to_qualify")
                .with_description(
                    "Seconds from the alert's creation in incident.io to signalman's decision",
                )
                .with_unit("s")
                .build(),
            typesafe_tokens: meter
                .u64_counter("signalman.typesafe.tokens")
                .with_description("Tokens per TypeSafe response, by model and direction")
                .with_unit("{token}")
                .build(),
            upstream_errors: meter
                .u64_counter("signalman.upstream.errors")
                .with_description("Failed attempts against an upstream, by service and status")
                .with_unit("{attempt}")
                .build(),
            webhook_deliveries: meter
                .u64_counter("signalman.webhook.deliveries")
                .with_description("incident.io webhook deliveries by result")
                .with_unit("{delivery}")
                .build(),
        }
    }
}

/// The process-wide instruments.
pub fn metrics() -> &'static Metrics {
    static METRICS: OnceLock<Metrics> = OnceLock::new();
    METRICS.get_or_init(Metrics::new)
}

/// One decided triage: the count, its duration, and the time to qualify
/// when the alert's creation time was known.
pub fn record_triage(
    decision: &str,
    team: Option<&str>,
    mode: &str,
    elapsed: Duration,
    time_to_qualify: Option<Duration>,
) {
    let m = metrics();
    let decision_attr = KeyValue::new("decision", decision.to_owned());
    m.triages.add(
        1,
        &[
            decision_attr.clone(),
            KeyValue::new("team", team.unwrap_or("none").to_owned()),
            KeyValue::new("mode", mode.to_owned()),
        ],
    );
    m.triage_duration
        .record(elapsed.as_secs_f64(), std::slice::from_ref(&decision_attr));
    if let Some(ttq) = time_to_qualify {
        m.time_to_qualify
            .record(ttq.as_secs_f64(), std::slice::from_ref(&decision_attr));
    }
}

/// signalman's [`judgment::Observer`]: the judgment crate reports token usage
/// and failed attempts here, and this module turns them into the instruments
/// above. Installed once by [`Providers::init`] as the process-wide observer,
/// so every client built anywhere in the process reports without wiring.
#[derive(Debug, Clone, Copy, Default)]
pub struct Observer;

impl judgment::Observer for Observer {
    fn on_usage(&self, model: &str, usage: &judgment::Usage) {
        record_typesafe_usage(model, usage.input_tokens, usage.output_tokens);
    }

    fn on_failed_attempt(&self, service: &'static str, status: &str) {
        record_upstream_error(service, status);
    }
}

/// One TypeSafe response's token usage.
pub fn record_typesafe_usage(model: &str, input_tokens: u64, output_tokens: u64) {
    let m = metrics();
    let model = KeyValue::new("model", model.to_owned());
    m.typesafe_tokens.add(
        input_tokens,
        &[model.clone(), KeyValue::new("direction", "input")],
    );
    m.typesafe_tokens.add(
        output_tokens,
        &[model, KeyValue::new("direction", "output")],
    );
}

/// One failed attempt against an upstream. `status` is the HTTP status
/// code, `transport` when no response came back, or, from the TypeSafe
/// client, `decode` (a 2xx whose body did not decode) or `unfit` (a 2xx that
/// did not fit the questions sent). The shared retry loop reports the first
/// two for every upstream; the TypeSafe client reports the last two itself,
/// since the loop saw a success.
pub fn record_upstream_error(service: &'static str, status: &str) {
    metrics().upstream_errors.add(
        1,
        &[
            KeyValue::new("service", service),
            KeyValue::new("status", status.to_owned()),
        ],
    );
}

/// One incident.io delivery, by what the receiver did with it.
pub fn record_webhook_delivery(result: &'static str) {
    metrics()
        .webhook_deliveries
        .add(1, &[KeyValue::new("result", result)]);
}

/// Export the receiver's in-flight triages as gauges,
/// `signalman.triage.inflight` by `state` (`running` or `queued`), read on
/// every metrics export for as long as `state` lives.
pub fn observe_inflight(state: &std::sync::Arc<AppState>) {
    let weak: Weak<AppState> = std::sync::Arc::downgrade(state);
    let _gauge = global::meter(SCOPE)
        .u64_observable_gauge("signalman.triage.inflight")
        .with_description("Triages running, and waiting for a slot, on this replica")
        .with_unit("{triage}")
        .with_callback(move |observer| {
            let Some(state) = weak.upgrade() else {
                return;
            };
            let running = state.running();
            let queued = state.admitted().saturating_sub(running);
            observer.observe(
                u64::try_from(running).unwrap_or(u64::MAX),
                &[KeyValue::new("state", "running")],
            );
            observer.observe(
                u64::try_from(queued).unwrap_or(u64::MAX),
                &[KeyValue::new("state", "queued")],
            );
        })
        .build();
}
