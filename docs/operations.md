---
title: Operations
description: Running the webhook receiver, its endpoints and manual commands, the upstream limits that bound throughput, what the logs contain, and how each failure shows up.
status: current
last_reviewed: 2026-09-20
tags: [operations]
---

# Operations

## Running

```sh
signalman serve --addr 0.0.0.0:8080                 # production shape
signalman serve --dry-run                            # decide, write nothing
signalman serve --notify-owners                      # also notify groups through Backstage
signalman serve --insecure-skip-verify               # local only; logs a warning on start
```

Startup fails fast when a required variable is missing. The process exits cleanly on SIGTERM or Ctrl-C. Backstage enrichment is on whenever `BACKSTAGE_BASE_URL` is set.

## Endpoints

| Path | Purpose |
|---|---|
| `POST /webhooks/incidentio` | delivery endpoint |
| `GET /healthz` | liveness; returns `ok` |

There is no readiness endpoint yet; upstream reachability is only known when a flow runs.

## Manual commands

```sh
signalman incidentio whoami                              # API key and roles
signalman incidentio open-incidents                      # dedup candidates as the flow sees them
signalman incidentio triage-alert <alert id> --dry-run   # the webhook flow for one alert, no write-back
signalman backstage lookup <component> --text "<alert>"  # what the catalog contributes
signalman triage examples/alerts/dns.json --print-request  # exact TypeSafe request, no call
```

`incidentio triage-alert` is also the way to retry a triage that failed after the webhook was acknowledged.

## Limits that bound throughput

| Limit | Value | Consequence |
|---|---|---|
| incident.io API key | 1,200 requests/min | shared by all calls |
| incident.io list incidents | 60 requests/min | one call per triage; the binding limit at volume |
| incident.io alert source events | 60 burst, 120/min per source | `--forward-to-incidentio` only |
| TypeSafe | 1,200 requests/min; 64k tokens per request | one request per triage, roughly 1 to 4k tokens with catalog context |
| Backstage | instance dependent | up to five catalog calls and one TechDocs call per triage |
| Background triage concurrency | unbounded today | see roadmap |

## Logs

`tracing` to stderr, filtered by `RUST_LOG` (default `info`). One line per triage carries the alert id, title, decision, resolved component, attachment, notification recipient, whether write-back applied, and the versioned model. Rejected webhooks log the reason and `webhook-id`. Retries log attempt, status and delay at `warn`. Catalog enrichment details are at `debug`.

## Failure modes

| Situation | Behaviour |
|---|---|
| Bad signature or missing headers | 401; incident.io retries for 24 h; fix the secret |
| Unparseable body | 400; incident.io retries; check the event subscription |
| Duplicate delivery | 200, no work |
| Component not in the catalog | triage continues with all team groups as candidates |
| Backstage transport or auth error | triage fails and is logged; the alert stays untagged |
| TypeSafe or incident.io error in the flow | logged at `error`; alert stays untagged; no retry because the delivery was already acknowledged |
| Notification fails | logged at `warn`; tags and attachment stay |
| Process restart | the in-memory `webhook-id` set is lost; a resend within the window is processed again; tag adds are idempotent |
