---
title: 0005 Observability signals enter through incident.io
description: Tsuga alerts reach signalman as incident.io alerts through Tsuga's own notification integration and are read back as related firing alerts; no direct Tsuga client is built until its API contract can be read with an operation API key.
status: accepted
date: 2026-09-20
decision-makers: [platform engineering]
consulted: [observability]
informed: [on-call leads]
last_reviewed: 2026-09-20
tags: [decisions, tsuga, incident.io, observability]
---

# 0005 Observability signals enter through incident.io

## Context and problem statement

Tsuga is the observability platform. Its monitors and SLOs raise alerts that, today, notify incident.io through Tsuga's native incident.io integration. Qualifying an alert is faster when the responder and the model see what else is firing on the same component and its neighbours. Should signalman read that context from Tsuga directly, or from incident.io?

What could be established about Tsuga's API without an account:

- An OpenAPI document exists at `GET https://api.tsuga.com/v0/open-api` (and `/v0/internal-open-api`). Both answer `401 UNAUTHORIZED` without credentials, and no public copy or documentation of the spec was found. An operation API key, created in the Tsuga admin centre, is needed to read it.
- The web application's bundles reference `POST /v0/alerts/ongoing/query`, paginated, with filters `owners` (team ids), `searchQuery` (substring over monitor id, name, query filters and aggregate fields), `priorities`, `clusterIds`, `triggeredBy` (`key:value` group-by dimensions) and `types` (`monitor`, `slo`), sortable by priority or start time. They also reference a service catalog under `/v0/service-catalog` and `GET /v0/alerts/statistics`.
- The authentication header that operation API keys use could not be confirmed; the web app authenticates with a session bearer token.

## Decision drivers

- Decision 0001: incident.io is the alert hub; every source lands there first
- A client built on a reverse-engineered contract breaks silently when the contract moves
- Each upstream adds credentials to hold, a failure mode to contain and a call per triage
- Tsuga already forwards alerts to incident.io; alerts that never notify are, by the team's own configuration, below the notification bar
- The tool should do few things well

## Considered options

1. Read related alerts from incident.io (`GET /v2/alerts?status[one_of]=firing&created_at[gte]=…`); Tsuga signals arrive through Tsuga's incident.io integration
2. A Tsuga client on the evidenced `POST /v0/alerts/ongoing/query`, filtered by the component name, behind a feature flag
3. Both: incident.io for what notified, Tsuga for what is firing below the notification bar

## Decision outcome

Chosen option: "Read related alerts from incident.io". It needs no new credential, no new failure mode and no unverified contract, and it applies to every source that feeds incident.io, not only Tsuga. The Tsuga integration is configured on the Tsuga side (notification rules → incident.io) so that monitor and SLO alerts carry the service name as an alert attribute, which signalman already reads as the component hint.

### Consequences

- Good, because one query gives the blast radius across every monitoring source in the hub
- Good, because signalman holds one upstream credential fewer and stays inside decision 0001
- Bad, because Tsuga alerts that do not notify incident.io are invisible to the triage; that is the team's notification policy, not a gap signalman fills silently
- Bad, because the window query is one page of at most 50 alerts; a large storm is sampled, not enumerated

### Confirmation

- `Triager::related_alerts` in `src/incidentio/sync.rs` is the only blast-radius lookup; no module talks to Tsuga
- `tests/webhook_server.rs` asserts that the firing-alert query is made and that `alert.related_alerts` reaches the model state
- Revisit is tracked as `signalman-3l7.6`: read `/v0/open-api` with an operation API key and decide whether option 3 earns its cost

## Pros and cons of the options

### Read related alerts from incident.io

- Good, because the contract is the documented OpenAPI v3 the rest of the integration already relies on
- Good, because it covers Datadog, Alertmanager, Tsuga and any other source alike
- Neutral, because the time window and cap are tunable per deployment
- Bad, because it sees only what notified

### A Tsuga client on the evidenced query

- Good, because it would show ongoing low-priority alerts and SLO burn on the same service
- Bad, because the contract was read from minified frontend code, not from a published spec
- Bad, because the authentication for operation API keys is unconfirmed
- Bad, because it adds a credential, a timeout budget and a retry path for a signal of unproven value

### Both

- Good, because it is the most complete picture
- Bad, because it carries every cost of the second option and duplicates alerts that notified, which must then be deduplicated by dedup key
