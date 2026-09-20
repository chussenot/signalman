---
title: Architecture
description: Components of rustsafe, the flow of an alert through them, and the boundaries between model judgment, code policy and incident.io.
status: current
last_reviewed: 2026-09-20
tags: [architecture]
---

# Architecture

## Components

```
 Datadog / Alertmanager / ...          ┌──────────────────────────┐
            │  alerts                   │        TypeSafe          │
            ▼                           │  POST /v1/systemone      │
 ┌──────────────────────┐   webhook     │  owner? impact? action-  │
 │      incident.io     │ ────────────▶ │  able? duplicate of?     │
 │  alerts · incidents  │  alert_created│  caused by change?       │
 │  alert routes        │ ◀──────────── └──────────────────────────┘
 │  escalations         │  tags + attach          ▲
 └──────────────────────┘                          │ one request
            ▲                                      │
            │  enriched alert event        ┌───────┴────────┐
            └──────────────────────────────│    rustsafe    │
               (CLI, optional)             │ serve · triage │
                                           └────────────────┘
```

| Component | Module | Responsibility |
|---|---|---|
| TypeSafe client | `src/client.rs`, `src/question.rs`, `src/answer.rs` | Wire contract, retries, typed questions and answers |
| Triage | `src/triage/` | The alert state, the fan-out questions, the routing policy |
| incident.io client | `src/incidentio/client.rs`, `types.rs` | Incidents, alerts, tags, attachments, alert-source events |
| Webhook verification | `src/incidentio/webhook.rs` | Svix signature check, event envelope parsing |
| Sync flow | `src/incidentio/sync.rs` | Fetch, judge, decide, write back |
| Receiver | `src/serve.rs` | HTTP endpoint, idempotency, background execution |
| Shared HTTP | `src/http.rs` | Retry policy and loop used by both clients |
| CLI | `src/main.rs` | `triage`, `models`, `serve`, `incidentio` |

## Flow of one alert

1. incident.io receives an alert from any source and emits `public_alert.alert_created_v1` to the webhook endpoint.
2. The receiver verifies the signature, drops duplicate deliveries by `webhook-id`, replies 202, and spawns the flow.
3. The flow fetches the alert by id and the incidents in `triage`, `live` and `paused` categories. Fetching fresh state is what makes delivery order irrelevant.
4. The alert and up to 40 candidate incidents become the `state`. One request asks every question the policy may need.
5. Answers are read through typed handles and passed to the policy, which returns one decision.
6. Tags are added to the alert. If the decision is to attach, the alert is connected to the chosen incident.

The CLI path is the same flow with the alert read from a file, candidates optionally pulled from the API, and the write-back replaced by printing or by forwarding to an HTTP alert source.

## Boundaries

**Model versus code.** The model answers questions whose answer depends on reading and understanding text. Code decides which questions exist, which candidates are offered, how answers combine, and what happens next. A threshold change never requires re-running the model.

**rustsafe versus incident.io.** rustsafe writes tags and attachments. incident.io owns incident creation, escalation, notification and the human workflow. This is recorded in [decision 0001](decisions/0001-incidentio-remains-the-alert-hub.md).

**Typed versus dynamic.** Question ids are only wire keys. The typed handle returned when a question is added carries the answer type, so a Choice over `Team` deserialises into `Team` and a mismatch is an error. Where options are only known at runtime, such as incident references, a dynamic Choice returns strings and the code maps them back.

## Failure posture

- A TypeSafe or incident.io error inside the flow is logged with the alert id; the webhook was already acknowledged, so incident.io does not retry. The alert simply carries no tags. Nothing is paged or suppressed on error.
- A signature failure is a 401 and incident.io retries for 24 hours, which is the intended behaviour for a misconfigured secret.
- Retries apply to transient statuses only, with jittered backoff and `Retry-After`.
