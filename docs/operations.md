---
title: Operations
description: Running the webhook receiver, its endpoints including liveness and readiness, its manual commands, the upstream limits that bound throughput, what the logs contain and where traces and metrics go, and how each failure shows up.
status: current
last_reviewed: 2026-09-25
tags: [operations]
---

# Operations

This page is for whoever runs the webhook receiver: what to start, what to probe, what bounds the load it can take, and how each failure shows up. The design constraints behind those answers live in the [decision records](decisions/README.md); this page states their conclusions and links them.

## Running

```sh
signalman serve --addr 0.0.0.0:8080                 # production shape
signalman serve --dry-run                            # decide, write nothing
signalman serve --notify-owners                      # also notify groups through Backstage
signalman serve --insecure-skip-verify               # local only; logs a warning on start
signalman serve --config /etc/signalman/config.toml  # explicit file; see Configuration for the search order
```

Startup fails fast on a missing secret, an unknown key in the configuration file or an unparsable environment value, so a misconfiguration stops the rollout instead of silently keeping a default ([decision 0006](decisions/0006-layered-configuration.md)). The process exits cleanly on SIGTERM or Ctrl-C. Backstage enrichment is on whenever `backstage.base_url` is set, in the file or through `BACKSTAGE_BASE_URL`.

## Endpoints

Every route exists for a different caller with a different credential: incident.io delivers alerts, the kubelet probes, delivery tooling posts changes, agents call tools. Keeping them apart means each credential can be rotated or revoked without touching the others, and a route that is not configured is not mounted rather than mounted and refusing.

| Path | Purpose |
|---|---|
| `POST /webhooks/incidentio` | delivery endpoint |
| `GET /healthz` | liveness; returns `ok` |
| `GET /readyz` | readiness; 200 when incident.io, TypeSafe and (when configured) Backstage answer with the configured credentials, 503 with a JSON body naming the failing upstream; cached 30 s ([decision 0009](decisions/0009-readiness-depends-on-the-upstreams.md)) |
| `POST /changes`, `GET /changes` | the [change feed](changes.md); routed only when `SIGNALMAN_CHANGES_TOKEN` is set; bearer token |
| `POST /changes/argocd`, `POST /changes/gitlab` | native adapters: Argo CD `Application` (bearer) and GitLab webhooks (`X-Gitlab-Token`) |
| `POST /mcp` | the [MCP server](mcp.md#transports) over Streamable HTTP; mounted only when `SIGNALMAN_MCP_TOKEN` is set; bearer token; stateless, no session id, so replicas need no session affinity |

## Readiness

The receiver acknowledges a delivery with `202` before it calls any upstream, so a replica with a revoked key or no route to TypeSafe would accept alerts it cannot triage and leave them untagged. [Decision 0009](decisions/0009-readiness-depends-on-the-upstreams.md) therefore makes readiness depend on the upstreams: `GET /healthz` says the process runs, `GET /readyz` says the upstreams this replica needs answer with the credentials it holds, and the probe fails when one does not. Each check is the cheapest authenticated call the upstream offers: incident.io `GET /v1/identity`, TypeSafe `GET /v1/models`, and, only when `backstage.base_url` is set, one catalog `by-query` for a single component. A wrong or revoked API key shows as not ready at rollout, not at the first alert.

The response is `200` when every upstream answered and `503` otherwise, with the same JSON body either way and `Cache-Control: no-store`:

```json
{
  "ready": false,
  "checked_at": "2026-09-23T03:31:28.749886571Z",
  "cached": false,
  "upstreams": [
    { "service": "typesafe", "ok": true, "latency_ms": 1 },
    { "service": "incidentio", "ok": false, "latency_ms": 1, "error": "incident.io rejected the credentials (401) [request_id r]" }
  ]
}
```

`error` is present only on a failed entry: the client's own error text, or `timed out after 3.0 s` when the deadline hit. The TypeSafe error text ends with `[request_id …]` when the API sent an `x-typesafe-request-id`, as incident.io's does with the request id from its error body: the id to quote to that vendor's support. `latency_ms` is how long that check took, whichever way it ended. The body has two entries, or three with Backstage.

Two settings shape the probe, both under `[server]` with the usual layers and no flags ([Configuration](configuration.md#server)):

| Setting | Default | Too low | Too high |
|---|---|---|---|
| `server.readiness_timeout_seconds` | `3` (at least 1) | shorter than a client's retry backoff, so a struggling upstream reports `timed out` instead of the transport error its last attempt saw | the probe answers late and the kubelet's own `timeoutSeconds` fires first, hiding which upstream was slow |
| `server.readiness_cache_seconds` | `30` (`0` checks on every request) | probe storms across replicas become load on rate-limited APIs, which is what the cache exists to prevent | a replica stays marked ready, or unready, for that long after the upstream changed state |

The checks run concurrently, each under its own deadline. The report is cached, failures included, and concurrent probes while a check is in flight wait for that one check rather than each running their own; `cached: true` marks a reused report. The trade-off the record accepts: during an incident.io or TypeSafe outage every replica goes unready, the Service stops routing deliveries signalman could not process anyway, and incident.io redelivers for 24 hours, at the price of no acknowledgement or deduplication until the upstream returns. Probe settings should tolerate a blip: `periodSeconds: 10`, `timeoutSeconds: 5`, `failureThreshold: 3` pull a replica after about 30 s of upstream failure. The MCP-only process (`signalman mcp` with `mcp.transport = "http"`) serves `/healthz` only.

Each uncached check runs inside a `readiness.check` span; the upstream calls it makes are the already instrumented ones, so a failed probe also counts in `signalman.upstream.errors` ([Observability](observability.md#spans)). A failed check logs one `warn` line naming the service and the error. Like the rest of the receiver, the endpoint is tested against wiremock only, never a live account.

## Manual commands

Each command runs one step of the flow by hand, against the live upstreams, so an operator can see what the receiver would have seen without waiting for a webhook.

```sh
signalman incidentio whoami                              # API key and roles
signalman incidentio open-incidents                      # dedup candidates as the flow sees them
signalman incidentio triage-alert <alert id> --dry-run   # the webhook flow for one alert, no write-back
signalman backstage lookup <component> --text "<alert>"  # what the catalog contributes
signalman triage examples/alerts/dns.json --print-request  # exact TypeSafe request, no call
signalman config show                                    # effective configuration after every layer
signalman eval examples/eval/cases.jsonl --record runs/x  # grade labelled alerts, keep raw responses
```

`incidentio triage-alert` is also the way to retry a triage that failed after the webhook was acknowledged.

## Kubernetes

A rollout is the unit of change: the shape of a deployment is a `ConfigMap`, the secrets are a `Secret`, and a change to either rolls the pods, so every configuration change is visible in the deployment history and there is nothing to hot-reload ([decision 0006](decisions/0006-layered-configuration.md)). Nothing below has been run against a cluster yet; it follows the configuration contract in [Configuration](configuration.md).

```yaml
apiVersion: v1
kind: ConfigMap
metadata:
  name: signalman-config
data:
  config.toml: |
    [server]
    addr = "0.0.0.0:8080"
    [backstage]
    base_url = "http://backstage-backend.backstage.svc:7007"
    app_url = "https://backstage.example.com"
    notify = true
    [policy]
    page_at = "major"
---
apiVersion: v1
kind: Secret
metadata:
  name: signalman-secrets
type: Opaque
stringData:
  TYPESAFE_API_KEY: "…"
  INCIDENTIO_API_KEY: "…"
  INCIDENTIO_WEBHOOK_SECRET: "whsec_…"
  BACKSTAGE_TOKEN: "…"
  SIGNALMAN_MCP_TOKEN: "…"                  # mounts /mcp on the same port; omit to serve no MCP over HTTP
---
apiVersion: apps/v1
kind: Deployment
metadata:
  name: signalman
spec:
  replicas: 2
  selector:
    matchLabels: { app: signalman }
  template:
    metadata:
      labels: { app: signalman }
      annotations:
        # Rendered by the pipeline (Helm: sha256sum of the ConfigMap); a
        # changed value rolls the pods, which is how a new config takes effect.
        checksum/config: "<sha256 of config.toml>"
    spec:
      containers:
        - name: signalman
          image: ghcr.io/example/signalman:0.4.0
          args: ["serve"]                       # finds /etc/signalman/config.toml
          envFrom:
            - secretRef: { name: signalman-secrets }
          env:
            - name: SIGNALMAN_RELATED_WINDOW_MINUTES   # a per-environment override, above the file
              value: "15"
            - name: HOST_IP
              valueFrom: { fieldRef: { fieldPath: status.hostIP } }
            - name: OTEL_EXPORTER_OTLP_ENDPOINT        # collector DaemonSet on the node; or a cluster Service URL; omit to export nothing
              value: "http://$(HOST_IP):4318"
          ports:
            - containerPort: 8080
          volumeMounts:
            - name: config
              mountPath: /etc/signalman
              readOnly: true
          livenessProbe:
            httpGet: { path: /healthz, port: 8080 }
          readinessProbe:
            httpGet: { path: /readyz, port: 8080 }    # upstream checks; see Readiness above and decision 0009
            periodSeconds: 10
            timeoutSeconds: 5
            failureThreshold: 3
      volumes:
        - name: config
          configMap: { name: signalman-config }
```

Validate the rendered file in the pipeline before applying: `signalman config show --config config.toml` exits non-zero on an unknown key, a wrong type or an out-of-range threshold.

The two replicas share nothing. For `/mcp` that is fine: every `POST` is one request and one response with no session, so the `Service` needs no session affinity. For the change feed it means each replica holds its own window, as the limits table below notes.

## Limits that bound throughput

Every number here is a ceiling something else imposes: an upstream's rate limit, a memory budget per replica, or a deadline chosen so that a stuck call cannot hold a slot. Knowing which one binds at a given alert volume says what to raise, and what cannot be raised from this side.

| Limit | Value | Consequence |
|---|---|---|
| incident.io API key | 1,200 requests/min | shared by all calls |
| incident.io list incidents | 60 requests/min | one call per triage; the binding limit at volume |
| incident.io list alerts | shared key limit | one call per triage for related firing alerts; `--related-window-minutes 0` removes it |
| incident.io alert notes | shared key limit | two calls per triage: list, then create or replace; `--no-note` removes both |
| incident.io alert source events | 60 burst, 120/min per source | `--forward-to-incidentio` only |
| TypeSafe | 1,200 requests/min; 64k tokens per request | one request per triage, roughly 1 to 4k tokens with catalog context |
| Backstage | instance dependent | up to five catalog calls and one TechDocs call per triage |
| Change feed | 1,000 newest changes per replica, in memory | older changes evicted; a restart forgets the window |
| Background triages | 8 running, 64 waiting per replica | a delivery beyond that gets `503` with `Retry-After: 30`; incident.io retries it, so the hub's retry is the backpressure |
| Triage deadline | 60 s per alert | the flow is dropped at the deadline and reported as an error; tags written before it stay |

## Backpressure

Without a bound, an alert storm would spawn one background triage per delivery until the replica ran out of memory or the incident.io key ran into its rate limit, and every triage would then fail. The bound turns a storm into slower qualification instead. A replica admits at most `server.max_concurrent_triages` running plus `server.max_queued_triages` waiting triages ([Configuration](configuration.md#server)). The next `alert_created` delivery is answered `503` with `Retry-After: 30` before it is marked seen, so incident.io's retry (up to 24 hours, with backoff) delivers it again when a slot is free. A refused delivery costs nothing but the retry; an alert storm therefore degrades to slower qualification, not to memory growth or a rate-limited incident.io key. The log line at `warn` carries the running and admitted counts.

Each triage runs under `server.triage_timeout_seconds`. The clients' own timeouts and retries bound every call; the deadline bounds their sum, so a stuck upstream cannot hold a slot forever. A triage that overruns is dropped and reported as a failed outcome; whatever it had already written (tags, an attachment) stays, and `incidentio triage-alert` re-runs it by hand.

## Logs

The log exists so that one alert's decision can be found and read after the fact, on a replica that exports nothing, with `grep`. Logs are one of three signals. Spans over each triage and metrics for the product and its upstreams are exported over OpenTelemetry when a collector endpoint is configured; [Observability](observability.md) covers both. The log is per replica and stays on stderr for the platform's log shipper; nothing below is exported by signalman.

`tracing` to stderr, filtered by `RUST_LOG` (default `info`). Every line carries the prefixes of the spans it was emitted in, `triage{alert_id=al-1}:triage.flow{alert_id=al-1}:` for a line from the flow, so the same names appear in the log and in a trace. One line per triage, `alert triaged`, carries seven flat fields for grepping and reading: `alert_id`, `title`, `decision`, `impact`, `time_to_qualify_seconds`, `applied` and `model`. `time_to_qualify_seconds` is a number, and is absent when the alert carried no creation time to measure from.

The same line carries `outcome`: [the outcome contract](triage.md#the-outcome-contract) compacted onto one line of JSON. Everything the line used to spell out separately is inside it, addressable by a documented path: the resolved component is `outcome.component`, the attachment is `outcome.writes.attached`, the notification recipient is `outcome.writes.notified`, the note is `outcome.writes.note`, and the counts that were logged are the lengths of `outcome.related_alerts` and `outcome.recent_changes`. A log pipeline can index every field of every decision without parsing prose.

signalman installs one subscriber with the human-readable `tracing` text formatter (and, when an OTLP endpoint is set, the OpenTelemetry layer beside it, which changes nothing in the log); there is no JSON log format setting yet. Under that formatter `outcome=` is appended to the line verbatim, so every `alert triaged` line grows by the whole document — about 2.5 kB for the smallest shape and more with catalog context, related alerts and typed changes — and there is no way to turn it off. Pipe stderr through a JSON parser on the `outcome=` value, and size the log budget for it.

Rejected webhooks log the reason and `webhook-id`. Retries log attempt, status and delay at `warn`. An answer of a kind the judgment crate in this build does not know logs `answer of a kind this client does not know; kept as Answer::Unknown` at `warn`, once per such answer, inside the `typesafe.evaluate` span, with `question` (the question id) and `kind` (TypeSafe's `type` for it, escaped and cut to 64 characters). It means TypeSafe answers with a primitive this build cannot read, and upgrading judgment, which is a new signalman build, is the remedy; the failure-mode table below says what happens to the triage meanwhile. A spent retry budget logs `retry budget spent; returning the last failure` at `warn`, with the attempt, the wait it refused, the time elapsed and the budget; only a client given a budget logs it, and signalman sets none. Catalog enrichment details are at `debug`.

## Failure modes

Each row says what the operator sees and whether the hub's retry, a manual re-run, or a fix on this side recovers it. The pattern behind the table: a delivery refused before acknowledgement is retried by incident.io for free; a triage that fails after acknowledgement is not retried automatically, because the hub already believes it was handled, and `incidentio triage-alert` re-runs it by hand.

| Situation | Behaviour |
|---|---|
| Bad signature or missing headers | 401; incident.io retries for 24 h; fix the secret |
| Unparseable body | 400; incident.io retries; check the event subscription |
| Duplicate delivery | 200, no work |
| Component not in the catalog | triage continues with all team groups as candidates |
| Backstage transport or auth error | triage fails and is logged; the alert stays untagged |
| TypeSafe or incident.io error in the flow | logged at `error`; alert stays untagged; no retry because the delivery was already acknowledged |
| TypeSafe returned an answer kind this build does not know | the `warn` line above, naming the question and the kind; an answer under an id signalman did not ask is kept and the triage goes on; under an asked id the client refuses the response (`AnswerTypeMismatch`, counted as `status="unfit"`), the triage is logged at `error` and leaves the alert untagged, as in the row above; upgrade judgment (a new signalman build), then re-run with `incidentio triage-alert` |
| TypeSafe answered outside the questions (an option not offered, a legend not the levels sent) | logged at `error` as `triage failed`, the message naming the question and the option or level and ending with TypeSafe's request id; the alert stays untagged with no note, and nothing is paged or suppressed; `signalman.upstream.errors{service="typesafe",status="unfit"}` counts it; the call is not retried (it was billed); recover with `signalman incidentio triage-alert <id>`, and report the request id to TypeSafe |
| Notification fails | logged at `warn`; tags and attachment stay |
| Process restart | the in-memory `webhook-id` set is lost; a resend within the window is processed again; tag adds are idempotent |
