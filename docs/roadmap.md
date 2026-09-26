---
title: Roadmap
description: What has not been verified against live systems, what is missing, and how the gaps map to the beads backlog.
status: current
last_reviewed: 2026-09-26
tags: [roadmap]
---

# Roadmap

The backlog is tracked in beads (`bd ready`). This page is the narrative index; the issue ids are the source of truth.

## Unverified

No write has reached a live incident.io alert yet; the reads have (`signalman-b11.2`, `.3`, 2026-09-23: `whoami`, the incidents list with its repeated-key filter, alerts, alert notes, and the three write endpoints answering validation errors rather than `403` to an empty body). TypeSafe has answered once (`signalman-b11.1`, 2026-09-23: `jev-latest` resolved to `jev-1.13.0`, the three example alerts decided as labelled, the raw responses kept under `examples/eval/runs/jev-1.13.0` for replay), and the Backstage catalog calls have, once (`signalman-b11.5`); the rest of the Backstage integration has not. Wire shapes follow the documented examples and OpenAPI specifications. Epic `signalman-b11` covers live verification:

- TypeSafe: done. Every answer parsed without a `Decode` error; owner, actionability, duplicate and change judgments matched the labels, impact did on two of three (the `dns` case scored `major` where the label says `outage`, `signalman-ufg.7` follows up).
- incident.io reads: done. The key's roles are listed by `whoami`; the repeated-key form of `status_category[one_of]` is honoured (`closed` alone returns only closed incidents, `triage` plus `live` only those two).
- A signed webhook end to end through Svix Play or ngrok.
- A real Backstage: the catalog is done (`relations` do come back when `fields` narrows them; groups there were `spec.type: squad`, which is why `backstage.group_types` exists). Still open, blocked on a token allowed on those plugins: the TechDocs search index on your storage backend and the Notifications endpoint with `accessRestrictions`.

## Hardening the receiver (`signalman-m28`)

Concurrency is bounded (running plus waiting triages per replica, `503` with `Retry-After` beyond) and every triage runs under a deadline ([Operations](operations.md#backpressure)). OpenTelemetry tracing and metrics are delivered (`signalman-m28.2`, [Observability](observability.md)): spans over the delivery, the triage and every upstream call, and metrics that export time to qualify and the queue depth rather than logging them, over OTLP/HTTP when a collector endpoint is set. Verified against a mock collector only, never a real one. The readiness endpoint is delivered (`signalman-m28.3`, [Operations](operations.md#readiness)): `GET /readyz` checks incident.io, TypeSafe and, when configured, Backstage with the configured credentials, under a per-check deadline and a cached report. The container image is delivered (`signalman-m28.4`, [Operations](operations.md#container-image)): built on every pull request, published to GHCR on a version tag, started locally but not yet pulled by a cluster. Open: a JSON log format (`signalman-m28.5`).

## Triage quality (`signalman-ufg`)

Thresholds are untuned defaults. The [evaluation harness](evaluation.md) exists and runs against mocks and the three example alerts; what is missing is labelled history to feed it. The plan: labelled history to feed it (`signalman-ufg.6`), threshold tuning from its report, and smarter candidate selection by team and recency. `recent_changes` now comes from the [change feed](changes.md) when delivery tools post to it. The same harness decides whether [Laya](laya.md), an open-weights System One model that already runs behind the configured endpoint, can replace Jev after fine-tuning (`signalman-ufg.5`).

## Backstage bridge (`signalman-90m`)

Delivered against mocks. Open: candidate filtering by domain or system for large catalogs; a frontend card showing signalman's last decision per component. `recent_changes` is not a catalog concern: [decision 0007](decisions/0007-changes-are-pushed-not-polled.md) rejected polling deployment plugins, so changes reach signalman through the [change feed](changes.md), where the open item is more native adapters (only Argo CD and GitLab have one; GitHub Actions posts the generic shape directly and Flux needs a relay to it).

## Mean time to qualify (`signalman-3l7`)

Delivered against mocks: the qualification note, related firing alerts in the state and the note, and the time to qualify in the outcome. Deferred: the on-call person for the owner in the note, through incident.io schedules mapped from a group annotation (`signalman-3l7.5`); a direct Tsuga client, which waits for an operation API key to read the spec at `/v0/open-api` (`signalman-3l7.6`, [decision 0005](decisions/0005-observability-signals-enter-through-incidentio.md)).

## incident.io coverage (`signalman-5s9`)

Incident-created events are parsed but not acted on; the intended first use is a severity suggestion posted as a timeline note. Resolved events are not forwarded by the CLI. Private alerts are ignored.

## Agents (`signalman-4gp`)

signalman is a tool for agents, not an agent ([decision 0008](decisions/0008-signalman-is-a-tool-for-agents.md)). Delivered: the versioned outcome contract, schema v1 ([Triage](triage.md#the-outcome-contract)); the [MCP server](mcp.md) with five read-only tools and the gated `apply_qualification` write tool, over stdio and Streamable HTTP (`signalman-4gp.5`, `.6`, `.7`); and the generated [`llms.txt`](llms.txt) and [`llms-full.txt`](llms-full.txt) (`signalman-4gp.3`), which closes the epic's planned scope. Open: agent-driven remediation, deferred with its unblock criteria in the decision record (`signalman-4gp.4`); the runbook section that applies to an alert as a typed Choice question, planned under the MTTQ epic (`signalman-3l7.8`).

## CI

GitHub Actions has never run in this repository: every job is refused at scheduling because of the account's billing state (`signalman-oqj`). The workflow uses GitHub-owned actions only and is expected to pass once billing is resolved.

## Smaller known gaps

- The in-memory `webhook-id` set does not survive restarts; tag adds are idempotent, so the consequence is a repeated model call.
- Without Backstage, the built-in fallback team list encodes one organisation's ownership model; `[[triage.teams]]` in the configuration file replaces it, and `[triage.text]` the impact rubric, but both remain guesses until tuned on real alerts.
