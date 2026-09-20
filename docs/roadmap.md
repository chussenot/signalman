---
title: Roadmap
description: What is unverified, what is missing, and how the gaps map to the beads backlog.
status: current
last_reviewed: 2026-09-20
tags: [roadmap]
---

# Roadmap

The backlog is tracked in beads (`bd ready`). This page is the narrative index; the issue ids are the source of truth.

## Unverified

Nothing has run against a live TypeSafe or incident.io account. Wire shapes are asserted against documented examples and the OpenAPI specification. Epic `rustsafe-b11` covers live verification:

- A real TypeSafe call over the example alerts, recording the versioned model and the distributions.
- `incidentio whoami` to confirm key scopes.
- The repeated-key form of `status_category[one_of]` on the incidents list. The most likely drift point.
- A signed webhook end to end through Svix Play or ngrok.

## Hardening the receiver (`rustsafe-m28`)

Background triage concurrency is unbounded and there is no request timeout, body limit, readiness endpoint, telemetry export or container image.

## Triage quality (`rustsafe-ufg`)

Thresholds are untuned defaults. The plan is an evaluation harness over labelled historical alerts, threshold tuning from its report, enrichment of `recent_changes` from deploy sources so the `caused_by_change` question is actually asked, and smarter candidate selection by team and recency.

## incident.io coverage (`rustsafe-5s9`)

Incident-created events are parsed but not acted on; the intended first use is a severity suggestion posted as a timeline note. Resolved events are not forwarded by the CLI. Private alerts are ignored. Alert route setup for the `ai-*` tags is undocumented beyond [the integration page](incidentio.md).

## Smaller known gaps

- `Retry-After` in HTTP-date form falls back to backoff.
- The in-memory `webhook-id` set does not survive restarts; tag adds are idempotent so the consequence is a repeated model call.
- The `Team` enum and impact rubric encode one organisation's ownership model.
