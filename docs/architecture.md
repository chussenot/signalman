---
title: Architecture
description: Components of signalman, the flow of an alert through them, and the boundaries between model judgment, code policy and incident.io.
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
            └──────────────────────────────│    signalman    │
               (CLI, optional)             │ serve · triage │
                                           └────────────────┘
```

| Component | Module | Responsibility |
|---|---|---|
| TypeSafe client | `src/client.rs`, `src/question.rs`, `src/answer.rs` | Wire contract, retries, typed questions and answers |
| Triage | `src/triage/` | The alert state, the fan-out questions, the routing policy |
| incident.io client | `src/incidentio/client.rs`, `types.rs` | Incidents, alerts, tags, attachments, alert-source events |
| Webhook verification | `src/incidentio/webhook.rs` | Svix signature check, event envelope parsing |
| Backstage bridge | `src/backstage/` | Catalog client, component resolution, owner candidates, TechDocs runbook, owner notification |
| Sync flow | `src/incidentio/sync.rs` | Fetch, enrich, judge, decide, write back |
| Receiver | `src/serve.rs` | HTTP endpoint, idempotency, background execution |
| Shared HTTP | `src/http.rs` | Retry policy and loop used by both clients |
| CLI | `src/main.rs` | `triage`, `models`, `serve`, `incidentio` |

## Flow of one alert

1. incident.io receives an alert from any source and emits `public_alert.alert_created_v1` to the webhook endpoint.
2. The receiver verifies the signature, drops duplicate deliveries by `webhook-id`, replies 202, and spawns the flow.
3. The flow fetches the alert by id and the incidents in `triage`, `live` and `paused` categories. Fetching fresh state is what makes delivery order irrelevant.
4. When Backstage is configured, the alert's service attribute is resolved to a catalog component; its record, its neighbours and its TechDocs runbook join the state, and the owner groups of that neighbourhood become the owner options ([Backstage bridge](backstage.md)). Otherwise the compiled-in team list is offered.
5. The alert and up to 40 candidate incidents become the `state`. One request asks every question the policy may need.
6. Answers are read through typed handles and passed to the policy, which returns one decision.
7. Tags are added to the alert. If the decision is to attach, the alert is connected to the chosen incident. If notifications are enabled, the owning group is told through Backstage.

The CLI path is the same flow with the alert read from a file, candidates optionally pulled from the API, and the write-back replaced by printing or by forwarding to an HTTP alert source.

## Boundaries

**Model versus code.** The model answers questions whose answer depends on reading and understanding text. Code decides which questions exist, which candidates are offered, how answers combine, and what happens next. A threshold change never requires re-running the model.

**signalman versus incident.io.** signalman writes tags and attachments. incident.io owns incident creation, escalation, notification and the human workflow. This is recorded in [decision 0001](decisions/0001-incidentio-remains-the-alert-hub.md).

**Catalog versus model.** The catalog states who owns what and what depends on what. The model judges which of those parties should take first response for this alert. Neither substitutes for the other; see [decision 0004](decisions/0004-catalog-is-the-ownership-source-of-truth.md).

**Typed versus dynamic.** Question ids are only wire keys. The typed handle returned when a question is added carries the answer type, so a Score cannot be read as a Noul and a mismatch is an error. Options that are only known at runtime, such as incident references and catalog groups, use a dynamic Choice that returns keys, and the code maps them back to typed candidates.

## Failure posture

- A TypeSafe or incident.io error inside the flow is logged with the alert id; the webhook was already acknowledged, so incident.io does not retry. The alert simply carries no tags. Nothing is paged or suppressed on error.
- A signature failure is a 401 and incident.io retries for 24 hours, which is the intended behaviour for a misconfigured secret.
- Retries apply to transient statuses only, with jittered backoff and `Retry-After`.
