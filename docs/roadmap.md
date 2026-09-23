---
title: Roadmap
description: What has not been verified against live systems, what is missing, and how the gaps map to the beads backlog.
status: current
last_reviewed: 2026-09-23
tags: [roadmap]
---

# Roadmap

The backlog is tracked in beads (`bd ready`). This page is the narrative index; the issue ids are the source of truth.

## Unverified

Nothing has run against a live TypeSafe, incident.io or Backstage instance. Wire shapes follow the documented examples and OpenAPI specifications. Epic `signalman-b11` covers live verification:

- A real TypeSafe call over the example alerts, recording the versioned model and the distributions.
- `incidentio whoami` to confirm key scopes.
- The repeated-key form of `status_category[one_of]` on the incidents list, the likeliest drift point.
- A signed webhook end to end through Svix Play or ngrok.
- A real Backstage: `relations` on `by-refs` responses when `fields` narrows them; the TechDocs search index on your storage backend; the Notifications endpoint with `accessRestrictions`.

## Hardening the receiver (`signalman-m28`)

Concurrency is bounded (running plus waiting triages per replica, `503` with `Retry-After` beyond) and every triage runs under a deadline ([Operations](operations.md#backpressure)). Open: OpenTelemetry tracing and metrics for the flow, so time to qualify and the queue depth are exported rather than logged; a readiness endpoint that checks upstream reachability; a container image and the CI job that builds it.

## Triage quality (`signalman-ufg`)

Thresholds are untuned defaults. The [evaluation harness](evaluation.md) exists and runs against mocks and the three example alerts; what is missing is labelled history to feed it. The plan: labelled history to feed it (`signalman-ufg.6`), threshold tuning from its report, and smarter candidate selection by team and recency. `recent_changes` now comes from the [change feed](changes.md) when delivery tools post to it. The same harness decides whether [Laya](laya.md), an open-weights System One model that already runs behind the configured endpoint, can replace Jev after fine-tuning (`signalman-ufg.5`).

## Backstage bridge (`signalman-90m`)

Delivered against mocks. Open: candidate filtering by domain or system for large catalogs; `recent_changes` from the `github`, `argocd` or `flux` plugins; a frontend card showing signalman's last decision per component.

## Mean time to qualify (`signalman-3l7`)

Delivered against mocks: the qualification note, related firing alerts in the state and the note, and the time to qualify in the outcome. Deferred: the on-call person for the owner in the note, through incident.io schedules mapped from a group annotation (`signalman-3l7.5`); a direct Tsuga client, which waits for an operation API key to read the spec at `/v0/open-api` (`signalman-3l7.6`, [decision 0005](decisions/0005-observability-signals-enter-through-incidentio.md)).

## incident.io coverage (`signalman-5s9`)

Incident-created events are parsed but not acted on; the intended first use is a severity suggestion posted as a timeline note. Resolved events are not forwarded by the CLI. Private alerts are ignored.

## Agents (`signalman-4gp`)

signalman is a tool for agents, not an agent ([decision 0008](decisions/0008-signalman-is-a-tool-for-agents.md)): no generative-model loop runs in the triage path, and what an assistant needs from signalman is its typed judgments, callable. Delivered: the outcome as a versioned JSON contract, schema v1, emitted by `signalman triage --json`, `signalman incidentio triage-alert` and the receiver's log line, with the schema committed under `docs/schema/` and a test that fails on drift ([Triage](triage.md#the-outcome-contract)); `signalman mcp`, five read-only tools over the same capabilities, served over stdio ([MCP server](mcp.md), `signalman-4gp.5`); and `apply_qualification`, a sixth tool that takes the outcome document back and re-derives every write from it, registered only when `mcp.allow_write` is true (default off, `signalman-4gp.6`); and Streamable HTTP as a second transport, stateless behind its own bearer token, mounted at `/mcp` by `serve` where it shares the change feed, or served alone by `signalman mcp` (`signalman-4gp.7`). Open: an `llms.txt` for this documentation (`signalman-4gp.3`). Agent-driven remediation is deferred with its unblock criteria written in the decision record (`signalman-4gp.4`). The runbook section that applies to an alert, the one investigation step with a measurable effect on time to qualify, is planned as a typed Choice question under the MTTQ epic (`signalman-3l7.8`).

## CI

GitHub Actions has never run in this repository: every job is refused at scheduling because of the account's billing state (`signalman-oqj`). The workflow uses GitHub-owned actions only and is expected to pass once billing is resolved.

## Smaller known gaps

- `Retry-After` in HTTP-date form falls back to backoff.
- The in-memory `webhook-id` set does not survive restarts; tag adds are idempotent, so the consequence is a repeated model call.
- Without Backstage, the built-in fallback team list encodes one organisation's ownership model; `[[triage.teams]]` in the configuration file replaces it, and `[triage.text]` the impact rubric, but both remain guesses until tuned on real alerts.
