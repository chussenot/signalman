---
title: Observability
description: What signalman exports over OpenTelemetry and when, the spans over one triage and its upstream calls, every metric with its attributes, how the OTLP endpoint is configured, how to verify the export against a local collector, and what is not instrumented.
status: current
last_reviewed: 2026-09-26
tags: [observability, operations, opentelemetry]
---

# Observability

signalman emits three signals: a text log on stderr, spans over each triage and its upstream calls, and metrics for the product and its upstreams. The log is always on. Spans and metrics are exported only when an OTLP endpoint is configured. Nothing here has been checked against a real collector or backend yet; the export is proven against a mock collector in the test suite ([Verifying locally](#verifying-locally)).

## What is exported and when

`telemetry.otlp_endpoint` (`OTEL_EXPORTER_OTLP_ENDPOINT`) is the gate. Unset or empty, the process installs only the text log: no exporter, no SDK thread, and every span and measurement below is a no-op. An existing deployment that does not set it sees no change.

With an endpoint, start-up installs a `tracing-opentelemetry` layer next to the text log and a global OpenTelemetry meter provider, and logs one `info` line, `OpenTelemetry export enabled (OTLP/HTTP)`, with the endpoint, the service name and the metrics interval. An endpoint that is not a URI fails start-up with a message naming the signal and the endpoint.

| Aspect | Behaviour |
|---|---|
| Protocol | OTLP/HTTP with protobuf bodies, sent by a blocking `reqwest` client on the SDK's own thread; no gRPC stack |
| Paths | `/v1/traces` and `/v1/metrics` appended to the configured base (a trailing slash is trimmed), as the OpenTelemetry specification does for the generic endpoint variable |
| Traces | batch span processor; every span is sampled, since the volume is one trace per alert |
| Metrics | cumulative temporality, the Prometheus-friendly default; exported every `telemetry.metrics_interval_seconds` (default 60) and once more at shutdown |
| Resource | `service.name` (configurable) and `service.version` (the crate version) on every span and metric |
| Flush | `main` shuts the providers down after every command, blocking until the last batch is sent or the SDK gives up. A one-shot run such as `signalman triage` or `signalman incidentio triage-alert` therefore exports its spans before exiting, not only `serve` |

A collector that is down or slow costs the triage nothing: spans and measurements are handed to the SDK and sent from its thread, so a failed export surfaces as the SDK's own warning in the log and never as a failed or delayed triage. A flush that does not complete at shutdown logs `metrics export did not flush cleanly` or `trace export did not flush cleanly` at `warn` and the process exits anyway.

## Configuration

The `[telemetry]` table follows the same layers as everything else, default, then file, then environment variable; there are no flags ([Configuration](configuration.md#telemetry)).

| File key | Environment | Default | Meaning |
|---|---|---|---|
| `telemetry.otlp_endpoint` | `OTEL_EXPORTER_OTLP_ENDPOINT` | unset | OTLP/HTTP base URL of a collector, for example `http://otel-collector.observability.svc:4318`; unset or empty exports nothing |
| `telemetry.service_name` | `OTEL_SERVICE_NAME` | `signalman` | `service.name` on every span and metric |
| `telemetry.metrics_interval_seconds` | `SIGNALMAN_METRICS_INTERVAL_SECONDS` | `60` | metrics export interval; at least 1 |

Which `OTEL_*` variables are honoured and which are not (the per-signal endpoints and the protocol are ignored; the header variables are read by the exporter itself and are secrets) is stated once, in [Configuration](configuration.md#telemetry), because [decision 0006](decisions/0006-layered-configuration.md) makes `src/config.rs` the one reader of non-secret variables and that page its reference.

## Spans

Spans come from `#[tracing::instrument]` on the flow and the client methods, so they exist whether or not an exporter is installed, and the text log shows them as prefixes on every line (`triage{alert_id=al-1}:triage.flow{alert_id=al-1}:`). With an exporter they reach the collector through `tracing-opentelemetry`. An error recorded on a span becomes an OpenTelemetry exception event on it.

| Span | Where | Fields |
|---|---|---|
| `webhook.receive` | one per incident.io delivery, in the receiver | `webhook_id`, `event_type`, `result`: `accepted`, `duplicate`, `ignored`, `rejected`, `invalid` or `refused` |
| `triage.background` | the triage spawned after the `202` | `alert_id`, `webhook_id` |
| `triage` | `Triager::triage_alert_by_id`: fetch the alert, then the flow | `alert_id` |
| `triage.flow` | `Triager::triage_alert`: enrich, ask, decide, write back | `alert_id`, `decision` (recorded at the end) |
| `incidentio.write_back` | the tags, attachment, note and notification writes; absent on a dry run | `tags` (count), `attach`, `note` (booleans) |
| `incidentio.get_alert`, `incidentio.list_incidents`, `incidentio.list_firing_alerts`, `incidentio.add_alert_tags`, `incidentio.attach_alert`, `incidentio.list_alert_notes`, `incidentio.create_alert_note`, `incidentio.update_alert_note`, `incidentio.send_alert_event`, `incidentio.identity` | one per incident.io call | the call's identifiers (`alert_id`, `max`, and so on); never the body |
| `typesafe.evaluate` | one per TypeSafe evaluation | `model`, `input_tokens` (recorded from the response; zero when the server reports none), `request_id`: the last attempt's `x-typesafe-request-id`, recorded on success, on an HTTP error and on a body that does not decode; absent after a transport failure or when the API sends none |
| `typesafe.list_models` | one per TypeSafe model listing (the readiness check) | `request_id`, as on `typesafe.evaluate` |
| `backstage.enrich`, `backstage.notify_owner` | catalog resolution and the owner notification | `hints`; `owner` |
| `readiness.check` | one per uncached `GET /readyz`, wrapping the upstream calls (`typesafe.list_models`, `incidentio.identity`, the Backstage query) | none |

`triage` and `triage.flow` sit on every path that qualifies an alert: the webhook, `signalman incidentio triage-alert`, and the MCP tool `qualify_alert` with an alert id. `signalman triage <file>` and `signalman eval` call the model directly, outside the `Triager`: they produce `typesafe.evaluate` (and `backstage.enrich` when a catalog is configured) but no `triage` or `triage.flow` span.

`triage.background` is not a child of `webhook.receive`. The receiver answers `202` and the request span ends, while the triage runs on for seconds. The triage span therefore *follows from* the request span, which OpenTelemetry renders as a span link: the trace of a delivery links to the trace of its triage, and a backend that shows links lets you cross from one to the other. The two are separate traces.

Never a span field: the qualification note's content, any request or response body, any secret. The note methods skip their `content` argument explicitly. The TypeSafe `request_id` is TypeSafe's own identifier for the call, not a secret: it is what TypeSafe support asks for, so a trace of a surprising decision leads straight to the call behind it. A value that is empty, not printable ASCII, or longer than 256 bytes is ignored rather than recorded.

## Metrics

Names are OpenTelemetry dotted names with a unit. A Prometheus exporter renders them with underscores and unit suffixes: `signalman.triage.count` becomes `signalman_triage_count_total`, `signalman.triage.duration` becomes `signalman_triage_duration_seconds` with its `_bucket`, `_sum` and `_count` series.

| Instrument | Kind | Unit | Attributes | Meaning |
|---|---|---|---|---|
| `signalman.triage.count` | counter | `{triage}` | `decision`, `team`, `mode` (`applied` or `dry_run`) | triages decided |
| `signalman.triage.duration` | histogram | `s` | `decision` | from the start of the triage to its decision and write-back done |
| `signalman.alert.time_to_qualify` | histogram | `s` | `decision` | from the alert's creation in incident.io to signalman's decision; recorded only when the alert carried `created_at` |
| `signalman.typesafe.tokens` | counter | `{token}` | `model`, `direction` (`input`, which is billed, or `output`) | TypeSafe token usage |
| `signalman.upstream.errors` | counter | `{attempt}` | `service` (`typesafe`, `incidentio`, `backstage`), `status` (the HTTP status, `transport`, `too_large`, or for `typesafe` `decode` and `unfit`) | every failed attempt, retried or not, counted in the shared retry loop, `too_large` being a body over the retry policy's 8 MiB cap, dropped unread and not retried; a 2xx the TypeSafe client could not use is counted by the client as `decode` (the body did not decode) or `unfit` (the answers did not fit the questions sent), neither retried |
| `signalman.webhook.deliveries` | counter | `{delivery}` | `result` (the `webhook.receive` values) | what the receiver did with each delivery |
| `signalman.triage.inflight` | observable gauge | `{triage}` | `state` (`running` or `queued`) | this replica's in-flight triages, read at each export; registered by `serve` only |

`team` is the owner key the decision chose, or `none` for a decision without an owner (suppress, attach). The three `signalman.triage.*` instruments and `signalman.alert.time_to_qualify` are recorded by the `Triager` flow only, the same paths as the `triage.flow` span; `signalman triage <file>` and `signalman eval` record `signalman.typesafe.tokens` and `signalman.upstream.errors` and nothing else. Every metric is one process's: sum counters and merge histograms across replicas in the backend, and sum the gauge for the fleet's queue depth. `signalman.triage.duration` measures signalman's own work; `signalman.alert.time_to_qualify` adds everything before it, incident.io's delivery delay and time spent waiting for a slot, which is why the two differ under load.

`signalman.alert.time_to_qualify` is the product metric: how long an alert waited before someone, or something, knew who owns it and what to do. Watch its p50 and p95 by `decision`, and set the objective on p95 (for example, 95% of alerts qualified within 60 seconds of creation). A rising p95 with a flat `signalman.triage.duration` points at delivery or queueing; a rising `signalman.triage.duration` points at an upstream, and `signalman.upstream.errors` by `service` says which. `signalman.typesafe.tokens` with `direction=input` is the billed volume.

## Logs

The text log on stderr is unchanged: `tracing`, filtered by `RUST_LOG` (default `info`), one `alert triaged` line per triage carrying the whole outcome document ([Operations](operations.md#logs)). Every line now carries its span prefixes, so `grep triage.flow` finds the flow's lines and `alert_id=` inside a prefix ties them to one alert. The log is per process and is not exported by signalman: metrics and spans are what leave the replica. There is no JSON log format yet (`signalman-m28.5`, open); there is no OpenTelemetry logs signal either.

## Verifying locally

The natural first check is the OpenTelemetry Collector with its debug exporter, which prints every span and metric it receives. This recipe follows the collector's documentation and has not been run here.

```yaml
# otel-collector.yaml
receivers:
  otlp:
    protocols:
      http:
        endpoint: 127.0.0.1:4318
exporters:
  debug:
    verbosity: detailed
service:
  pipelines:
    traces:
      receivers: [otlp]
      exporters: [debug]
    metrics:
      receivers: [otlp]
      exporters: [debug]
```

```sh
otelcol --config otel-collector.yaml &
export OTEL_EXPORTER_OTLP_ENDPOINT=http://127.0.0.1:4318
signalman triage examples/alerts/dns.json                 # one typesafe.evaluate span, the token counter
signalman incidentio triage-alert <alert id> --dry-run    # the whole flow: triage, triage.flow, the client spans, the triage metrics
```

Both commands need their API keys, which is why this has not been run here. The first should print a `typesafe.evaluate` span and, at exit, one export of `signalman.typesafe.tokens`. The second should print `triage.flow` inside `triage` with the incident.io and TypeSafe spans below them, no `incidentio.write_back` since it is a dry run, and an export of `signalman.triage.count` with `mode=dry_run`, `signalman.triage.duration` and, when the alert carries `created_at`, `signalman.alert.time_to_qualify`. With `serve` and a delivered webhook, `webhook.receive` and `triage.background` appear as two traces linked to each other, and `signalman.triage.inflight` is exported at every interval.

What the test suite proves: `tests/telemetry.rs` starts a `wiremock` server standing in for an OTLP/HTTP collector, runs a real triage against mock TypeSafe and incident.io with the endpoint pointed at it, shuts the providers down, and asserts that the protobuf `POST /v1/traces` and `POST /v1/metrics` bodies name the spans, the instruments and the attribute values above. It also asserts that a second `Providers::init` in the same process is refused. It does not decode the protobuf or exercise a real collector, so header handling, batching under load and how a backend renders the follows-from link remain unverified.

## What is not here

- No gRPC transport, and no `OTEL_EXPORTER_OTLP_PROTOCOL`: OTLP/HTTP with protobuf bodies only.
- No OpenTelemetry logs signal: the log stays on stderr, for the platform's log shipper.
- No trace-context propagation into or out of HTTP requests. incident.io does not send `traceparent` on its webhooks, and the three clients do not inject one, so a signalman trace begins at the delivery and does not continue into TypeSafe, incident.io or Backstage.
- No per-signal endpoints: `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` and `OTEL_EXPORTER_OTLP_METRICS_ENDPOINT` are ignored.
- No sampling knob: every span is exported. Volume is bounded by the alert rate, one trace per alert.
- No metrics without an endpoint: there is no Prometheus scrape endpoint on the process. Point the collector's Prometheus exporter at the OTLP metrics instead.
