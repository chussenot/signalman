---
title: 0009 Readiness depends on the upstreams
description: GET /readyz fails when TypeSafe, incident.io or a configured Backstage does not answer with this replica's credentials, so a bad key or an upstream outage takes the replica out of the Service instead of letting it accept deliveries it cannot process.
status: accepted
date: 2026-09-23
decision-makers: []
consulted: []
informed: []
last_reviewed: 2026-09-23
tags: [decisions, operations, readiness]
---

# 0009 Readiness depends on the upstreams

## Context and problem statement

The webhook receiver answers `202` to a delivery and runs the triage afterwards, so the delivery is acknowledged before any upstream is called. With a readiness probe that only said the process was running, a replica with a revoked API key, a wrong key for the environment, or no route to TypeSafe or incident.io kept taking deliveries it could not process: every triage failed after the acknowledgement, the alerts stayed untagged, and the failure was found at the first alert rather than at rollout. What should `GET /readyz` ([Operations](../operations.md#readiness), `src/readiness.rs`) check, and what should happen to traffic when a check fails?

## Decision drivers

- A misconfigured replica must be found at rollout, before it acknowledges an alert it cannot triage
- Readiness must not become load on the upstreams: incident.io rate-limits the API key, and Kubernetes probes every replica every few seconds
- incident.io retries a delivery that is refused or unanswered for 24 hours, so a refused delivery is delayed, not lost ([decision 0001](0001-incidentio-remains-the-alert-hub.md))
- A probe must answer quickly and predictably whatever the upstreams do

## Considered options

1. Liveness only: `/readyz` says the process runs and nothing more
2. Upstream checks on every probe, no cache
3. Upstream checks with a cached report, and the probe fails when an upstream does
4. Upstream checks with a cached report, reported in the body but never failing the probe

## Decision outcome

Chosen option: "Upstream checks with a cached report, and the probe fails when an upstream does". `GET /readyz` runs the cheapest authenticated call each configured upstream offers: TypeSafe `GET /v1/models`, incident.io `GET /v1/identity`, and one Backstage catalog `by-query` when `backstage.base_url` is set. The checks run concurrently, each under `server.readiness_timeout_seconds`, through the same clients a triage uses, credentials and retry policy included. The report is cached for `server.readiness_cache_seconds`, failures as well as successes, and concurrent probes wait for the one check in flight rather than each running their own. The response is `200` when every upstream answered and `503` otherwise, with the same JSON body naming each upstream, its latency and its error.

### Consequences

- Good, because a wrong or revoked key shows as not-ready at rollout, and the body says which upstream and why
- Good, because during a TypeSafe or incident.io outage every replica goes unready and the Service stops routing deliveries signalman could not process anyway; incident.io retries non-2xx responses and connection failures for 24 hours, so nothing is lost and recovery is automatic when the upstream returns
- Good, because the cache bounds readiness load to one call per upstream per replica per cache period, whatever the probe frequency
- Bad, because during such an outage the receiver also stops acknowledging and deduplicating: an alert that arrives then waits for the retry instead of being seen at once. This is accepted: a triage it cannot complete has no value, and the retry is free
- Bad, because a probe sees a stale answer for up to the cache period; readiness is meant to be slow-moving, and `server.readiness_cache_seconds = 0` disables the cache where that matters
- Bad, because a deadline shorter than a client's retry backoff reports `timed out` where a longer one would name the transport error the last attempt saw

Probe settings should tolerate a blip rather than react to one: `periodSeconds: 10`, `timeoutSeconds: 5`, `failureThreshold: 3` pull a replica after about 30 s of upstream failure, which with the default 30 s cache means one to two fresh checks.

### Confirmation

- `src/readiness.rs` is the only place a readiness check is defined; it calls the clients' existing methods (`list_models`, `identity`, `query`) and adds no request of its own
- `tests/readiness.rs` exercises `GET /readyz` against wiremock: `200` with every upstream up, two entries without a catalog and three with one, `503` naming the failing upstream, a slow upstream reported as `timed out` on its own deadline, the report reused within the cache window and re-run with the cache at zero, and `Cache-Control: no-store` on every answer
- `server.readiness_cache_seconds` and `server.readiness_timeout_seconds` follow [decision 0006](0006-layered-configuration.md); `tests/config_precedence.rs` covers the environment layer

## Pros and cons of the options

### Liveness only

- Good, because it never fails for a reason outside the process
- Good, because it costs nothing
- Bad, because a bad credential or an unreachable upstream is found at the first alert, after the delivery was acknowledged, with the alert left untagged

### Upstream checks without a cache

- Good, because every probe answer is current
- Bad, because probe storms multiply across replicas: every `periodSeconds` each replica makes one call per upstream, and incident.io rate-limits the key those calls share with the triages
- Bad, because a kubelet retrying a slow probe adds load exactly when the upstream is struggling

### Upstream checks with a cache, failing the probe

- Good, because a misconfigured replica is pulled before it acknowledges anything, and an outage pulls the fleet and hands the deliveries to incident.io's retry
- Good, because load is bounded by the cache period, not the probe period
- Bad, because acknowledgement and deduplication stop during an upstream outage, and a probe can be up to one cache period stale

### Upstream checks with a cache, reporting but never failing

- Good, because the `202` stays available during an upstream outage and the body still tells an operator what is wrong
- Bad, because deliveries keep flowing to replicas whose triages will fail after acknowledgement, which is the situation this record exists to end
- Rejected for now. It is a one-line change in the handler (`200` regardless of `ready`) if operations come to prefer availability of the acknowledgement over correctness of the triage; the evidence for that would be alerts that incident.io failed to redeliver, or deduplication lost during outages, neither of which has been observed

## More information

[Operations](../operations.md#readiness) documents the endpoint, the body and the settings; [Observability](../observability.md#spans) the `readiness.check` span and the `signalman.upstream.errors` counter a failed probe increments. Beads: `signalman-m28.3`. Like the rest of the receiver, the endpoint has been tested against wiremock only, never against a live account.
