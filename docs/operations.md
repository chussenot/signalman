---
title: Operations
description: Running the webhook receiver, health checks, rate limits, logs, and what happens when an upstream fails.
status: current
last_reviewed: 2026-09-20
tags: [operations]
---

# Operations

## Running

```sh
signalman serve --addr 0.0.0.0:8080          # production shape
signalman serve --dry-run                     # decide, write nothing
signalman serve --insecure-skip-verify        # local only; logs a warning on start
```

The process exits cleanly on SIGTERM or Ctrl-C. Startup fails fast when a required variable is missing.

## Endpoints

| Path | Purpose |
|---|---|
| `POST /webhooks/incidentio` | delivery endpoint |
| `GET /healthz` | liveness; returns `ok` |

There is no readiness endpoint yet; upstream reachability is only known when a flow runs.

## Manual runs

```sh
signalman incidentio whoami                          # key and roles
signalman incidentio open-incidents                  # dedup candidates as the flow would see them
signalman incidentio triage-alert <alert id> --dry-run
signalman triage examples/alerts/dns.json --print-request   # exact TypeSafe request, no call
```

## Limits to keep in mind

| Limit | Value | Effect |
|---|---|---|
| incident.io key | 1,200 requests/min | shared by all calls |
| incident.io list incidents | 60 requests/min | one call per triage; the binding limit at volume |
| incident.io alert source events | 60 burst, 120/min per source | `--forward-to-incidentio` |
| TypeSafe | 250,000 tokens/s, 1,200 requests/min, 64k tokens per request | one request per triage, roughly 1–3k tokens |
| Background triage concurrency | unbounded today | see roadmap |

## Logs

`tracing` to stderr, filtered by `RUST_LOG`. Every triage logs alert id, title, decision, attachment, whether write-back applied, and the versioned model. Rejected webhooks log the reason and `webhook-id`. Retries log attempt, status and delay at `warn`.

## Failure modes

| Situation | Behaviour |
|---|---|
| Bad signature or missing headers | 401; incident.io retries for 24 h; fix the secret |
| Unparseable body | 400; incident.io retries; check the event type subscription |
| TypeSafe or incident.io error during the flow | logged at `error`; alert stays untagged; no retry (already acknowledged) |
| Duplicate delivery | 200, no work |
| Process restart | in-memory `webhook-id` set is lost; a resend within the window is processed again, and tag adds are idempotent |
