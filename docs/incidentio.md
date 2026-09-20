---
title: incident.io integration
description: How signalman verifies and processes incident.io webhooks, what it reads and writes through the API including the qualification note, how to configure alert routes on the result, and how the CLI forwards enriched alerts.
status: current
last_reviewed: 2026-09-20
tags: [incidentio, webhooks]
---

# incident.io integration

incident.io is the alert hub. Alerts from every source land there first; signalman reacts to them and writes judgments back as tags and attachments. It never creates incidents ([ADR 0001](decisions/0001-incidentio-remains-the-alert-hub.md)).

Contract sources: the [OpenAPI v3 specification](https://api.incident.io/v1/openapiV3.json), the [documentation index](https://docs.incident.io/llms.txt) and the [webhook guide](https://docs.incident.io/api-reference/webhooks.md).

## Receiving a webhook

```mermaid
flowchart TD
    P[POST /webhooks/incidentio] --> V{verification<br/>disabled?}
    V -->|no| H{webhook-id, -timestamp,<br/>-signature present?}
    H -->|no| U401[401]
    H -->|yes| T{timestamp within<br/>5 min of now?}
    T -->|no| U401
    T -->|yes| S{any v1 signature equals<br/>HMAC-SHA256 of id.ts.body?}
    S -->|no| U401
    S -->|yes| D
    V -->|yes| D{webhook-id seen<br/>in last 4096?}
    D -->|yes| OK200[200, no work]
    D -->|no| E{body parses as<br/>event_type + payload?}
    E -->|no| B400[400]
    E -->|yes| K{event type}
    K -->|public_alert.alert_created_v1| SP[spawn triage by alert id] --> A202[202 Accepted]
    K -->|anything else| OK200b[200, ignored]
```

The signature is Svix's: HMAC-SHA256 over `id.timestamp.raw body`, keyed with the base64 part of the `whsec_` secret, compared in constant time; several space-separated `v1,` signatures are accepted during rotation, and both `webhook-*` and `svix-*` header names are read. The implementation is pinned to Svix's published test vector.

Two responses are refusals, 401 and 400, and incident.io retries them for 24 hours. Everything else is acknowledged, including duplicates and event types signalman does not handle, because a retry would only repeat the same delivery. Private-resource events carry an id only and are ignored.

## The flow after acknowledgement

The receiver answers 202 before any upstream call. The flow then fetches the alert by id and the incidents in `triage`, `live` and `paused` categories, so late or reordered deliveries cannot apply a stale decision. It also lists the alerts that are firing in the recent window (`GET /v2/alerts?status[one_of]=firing&created_at[gte]=…`, one page of 50, default window 30 minutes) and puts up to 20 of them, newest first and without the alert itself, into the state as `related_alerts`. That lookup is context, not a prerequisite: if it fails, the triage continues with an empty list and a warning. The rest is the [triage](triage.md), optionally preceded by [catalog enrichment](backstage.md).

Alerts from any source that notifies incident.io appear in that list. Tsuga, the observability platform, forwards its monitor and SLO alerts through its own incident.io integration; signalman does not talk to Tsuga directly ([decision 0005](decisions/0005-observability-signals-enter-through-incidentio.md)).

## What is written back

| Write | Endpoint | When |
|---|---|---|
| Tags `ai-team-<key>`, `ai-impact-<level>`, `ai-action-<decision>` | `POST /v2/alerts/{id}/actions/add_tags` | every triaged alert |
| Tag `ai-dup-<reference>` and the attachment | `add_tags` and `POST /v2/incident_alerts` | decision is attach |
| Tag `ai-suspected-change` | `add_tags` | `caused_by_change` above threshold |
| The qualification note | `POST /v1/alert_notes`, or `PUT /v1/alert_notes/{id}` on a later pass | every triaged alert unless `--no-note` |

Tag names are lowercase with hyphens and prefixed `ai-`, so alert routes can filter on them and humans can tell them from their own tags. `<key>` is the catalog group name when Backstage is configured, or the static team key otherwise. Existing tags are kept. `--dry-run` on `serve` and `incidentio triage-alert` computes everything and writes nothing.

### The qualification note

Tags carry the verdict; the note carries what a responder needs to trust it and act, in the alert view they already have open. It is rendered from a fixed template in code, never by the model ([decision 0002](decisions/0002-calibrated-judgments-over-generated-text.md)), and reads:

```markdown
**Signalman qualification**

**Decision:** Page **Payments** (Major impact). Owner confidence is moderate: confirm ownership.

- Owner: [Payments](https://backstage.example.com/catalog/default/group/payments) (confidence 0.62; next: Data Platform 0.31)
- Impact: Major (P 0.71; Outage 0.22)
- Actionable: 0.95
- Duplicate of: none of the 3 open incidents offered (confidence 0.80)
- Component: [checkout-api](https://backstage.example.com/catalog/default/component/checkout-api) (service, production), registered owner Payments; depended on by component storefront
- Runbook: [TechDocs page](https://backstage.example.com/docs/default/component/checkout-api/runbooks/high-error-rate/)
- Related firing alerts (last 30 min): 2
  - HighLatency payments-gateway (4 min ago, payments-gateway)
  - PodRestarts checkout-api (11 min ago, checkout-api)
- Upstream: https://app.datadoghq.eu/monitors/2

_Time to qualify: 42 s. Model jev-1.13.0. Tags: ai-team-payments, ai-impact-major, ai-action-page._
```

Every line is a judgment with its probability, a link, or a fact from the catalog or the hub. Alternatives are listed when they carry at least 0.05 probability, so a close call reads as one. The time to qualify is measured from the alert's `created_at` in incident.io to the decision and is also logged and returned in the outcome as `time_to_qualify_seconds`; it is the number this tool exists to lower.

The note starts with a fixed marker line. Before writing, signalman lists the alert's notes and, when one of them starts with the marker, replaces that note instead of adding another; a human's notes are never touched. One alert therefore carries at most one signalman note, always the latest pass. Writing the note requires the manage alert notes scope. A failure to write it is logged and does not undo the tags or the attachment.

## Setup

1. Create an API key with view alerts, view incidents, manage alert tags, manage incident alerts and manage alert notes. `signalman incidentio whoami` prints the roles the key has.
2. Run the receiver where incident.io can reach it; for local work, Svix Play or ngrok.
3. Settings → Webhooks → add endpoint, subscribe to **Alert created (public)**, copy the signing secret into `INCIDENTIO_WEBHOOK_SECRET`.
4. Send a test event from the webhook settings page. A signature failure is a 401 with the reason in the body.
5. Fire a real alert and confirm tags appear on it.

## Alert routes

Tags are the handoff. Typical routes:

| Condition | Action in incident.io |
|---|---|
| `ai-action-page` and `ai-team-<group>` | escalate to that group's on-call |
| `ai-action-ticket` | create a low-urgency incident or a ticket, no page |
| `ai-action-suppress` | do not create an incident; keep the alert for review |
| `ai-action-human-triage` | route to the triage channel |
| `ai-action-attach` | nothing; the attachment is already made |

Tags arrive a few seconds after the alert is created. Routes should evaluate on alert update as well as creation, or wait for the tag. This has not yet been verified against a live account.

## Forwarding from the CLI

`triage --forward-to-incidentio` posts the alert to an HTTP alert source (`POST /v2/alert_events/http/{config id}`), authenticated with the source's token rather than the API key. The judgments travel under `metadata.ai` (`team`, `team_confidence`, `impact_level`, `impact_label`, `impact_score`, `actionable`, `decision`, `model`), and the resolved component under `metadata.component`. Map them to alert attributes in the source's template, then route on the attributes. Only `status: firing` is sent today.

## Candidate incidents

`GET /v2/incidents` with `status_category[one_of]` for each of `triage`, `live` and `paused` as repeated keys, paginated with the `after` cursor, filtered client-side to `mode: standard`, capped at 40. The multi-value filter form is inferred from single-value examples and is unverified; the fallback is one request per category. The list endpoint has its own limit of 60 requests per minute.

## Errors

`incidentio::Error` maps 401, 403, 404, 422 and 429 to variants carrying the `request_id` and, for 422, the field-level messages from the documented error body. `Retry-After` on 429 is honoured. Everything else is `Http` with the truncated body.
