---
title: Roadmap
description: What has not been verified against live systems, what is missing, and how the gaps map to the beads backlog.
status: current
last_reviewed: 2026-09-20
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

Background triage concurrency is unbounded and there is no request timeout, body limit, readiness endpoint, telemetry export or container image.

## Triage quality (`signalman-ufg`)

Thresholds are untuned defaults. The plan: an evaluation harness over labelled historical alerts, threshold tuning from its report, `recent_changes` from deployment sources so `caused_by_change` is actually asked, and smarter candidate selection by team and recency.

## Backstage bridge (`signalman-90m`)

Delivered against mocks. Open: candidate filtering by domain or system for large catalogs; `recent_changes` from the `github`, `argocd` or `flux` plugins; a frontend card showing signalman's last decision per component.

## Mean time to qualify (`signalman-3l7`)

Delivered against mocks: the qualification note, related firing alerts in the state and the note, and the time to qualify in the outcome. Deferred: the on-call person for the owner in the note, through incident.io schedules mapped from a group annotation (`signalman-3l7.5`); a direct Tsuga client, which waits for an operation API key to read the spec at `/v0/open-api` (`signalman-3l7.6`, [decision 0005](decisions/0005-observability-signals-enter-through-incidentio.md)).

## incident.io coverage (`signalman-5s9`)

Incident-created events are parsed but not acted on; the intended first use is a severity suggestion posted as a timeline note. Resolved events are not forwarded by the CLI. Private alerts are ignored.

## CI

GitHub Actions has never run in this repository: every job is refused at scheduling because of the account's billing state (`signalman-oqj`). The workflow uses GitHub-owned actions only and is expected to pass once billing is resolved.

## Smaller known gaps

- `Retry-After` in HTTP-date form falls back to backoff.
- The in-memory `webhook-id` set does not survive restarts; tag adds are idempotent, so the consequence is a repeated model call.
- Without Backstage, the built-in fallback team list encodes one organisation's ownership model; `[[triage.teams]]` in the configuration file replaces it, and `[triage.text]` the impact rubric, but both remain guesses until tuned on real alerts.
