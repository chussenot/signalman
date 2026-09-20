---
title: incident.io integration
description: How signalman receives incident.io webhooks, what it reads and writes through the API, and how to configure alert routes to act on the result.
status: current
last_reviewed: 2026-09-20
tags: [incidentio, webhooks]
---

# incident.io integration

incident.io is used as the alert hub. Alerts from every source land there first; signalman reacts to them and writes judgments back. It never creates incidents ([decision 0001](decisions/0001-incidentio-remains-the-alert-hub.md)).

Contract sources: the [OpenAPI v3 specification](https://api.incident.io/v1/openapiV3.json), the [documentation index](https://docs.incident.io/llms.txt) and the [webhook guide](https://docs.incident.io/api-reference/webhooks.md).

## Webhook flow

1. incident.io delivers `public_alert.alert_created_v1` to `POST /webhooks/incidentio`.
2. The receiver verifies the Svix signature over the raw body. Headers are `webhook-id`, `webhook-timestamp`, `webhook-signature` (Svix's `svix-*` names are also accepted). The signed content is `id.timestamp.body`, the algorithm HMAC-SHA256, the key the base64 part of the `whsec_` secret. Several space-separated `v1,` signatures are accepted during rotation. The timestamp must be within five minutes.
3. Deliveries with an already seen `webhook-id` are acknowledged with 200 and ignored. Svix resends with the same id.
4. The receiver replies 202 and runs the flow in the background. incident.io retries non-2xx for 24 hours, so only a bad signature (401) or an unparseable body (400) is refused.
5. The flow fetches the alert by id and the incidents in `triage`, `live` and `paused` categories, judges, decides, and writes back.

Other event types are acknowledged and ignored. Private-resource events carry only an id and are ignored.

## What is written back

| Write | Endpoint | When |
|---|---|---|
| Tags `ai-team-<team>`, `ai-impact-<level>`, `ai-action-<decision>` | `POST /v2/alerts/{id}/actions/add_tags` | every triaged alert |
| Tag `ai-dup-<reference>` and attachment | `add_tags` and `POST /v2/incident_alerts` | decision is attach |
| Tag `ai-suspected-change` | `add_tags` | `caused_by_change` above threshold |

Tag names are lowercase with hyphens, prefixed `ai-` so they can be filtered and told apart from human tags. With Backstage configured, `<team>` is the catalog group name. Existing tags are kept.

`--dry-run` on `serve` and `incidentio triage-alert` computes everything and writes nothing.

## Setup

1. Create an API key with view alerts, view incidents, manage alert tags and manage incident alerts. `signalman incidentio whoami` prints the roles the key has.
2. Run the receiver where incident.io can reach it. For local work, Svix Play or ngrok.
3. Settings → Webhooks → add endpoint, subscribe to **Alert created (public)**, copy the signing secret into `INCIDENTIO_WEBHOOK_SECRET`.
4. Send a test event from the webhook settings page. A signature failure is a 401 with the reason in the body.
5. Fire a real alert and confirm tags appear on it.

## Alert routes

Tags are the handoff. Typical routes:

- `ai-action-page` and `ai-team-database` → escalate to the database on-call.
- `ai-action-suppress` → do not create an incident; keep the alert for review.
- `ai-action-human-triage` → a triage channel.
- `ai-action-attach` needs no route; the attachment is already made.

Tags arrive a few seconds after the alert. Routes should be configured to evaluate on alert update as well as creation, or to wait for the tag. This has not yet been verified against a live account.

## Forwarding from the CLI

`triage --forward-to-incidentio` posts the alert to an HTTP alert source with `POST /v2/alert_events/http/{config id}`, authenticated with the source's token rather than the API key. The judgments travel under `metadata.ai` (`team`, `team_confidence`, `impact_level`, `impact_label`, `impact_score`, `actionable`, `decision`, `model`). Map them to alert attributes in the source's template, then route on the attributes. Only `status: firing` is sent today.

## Candidate incidents

`GET /v2/incidents` with `status_category[one_of]` for each of `triage`, `live`, `paused`, sent as repeated keys, paginated with the `after` cursor, and filtered client-side to `mode: standard`. Up to 40 are offered as dedup options. The multi-value filter form is inferred from the single-value examples in the documentation and is unverified; if the API rejects it, one request per category is the fallback.

The list endpoint has its own limit of 60 requests per minute, separate from the 1,200 per minute key limit. At high alert volume, candidate lookup should be cached or narrowed by team.

## Errors

`incidentio::Error` maps 401, 403, 404, 422 and 429 to variants carrying the `request_id` and, for 422, the field-level messages from the documented error body. `Retry-After` on 429 is honoured. Everything else is `Http` with the truncated body.
