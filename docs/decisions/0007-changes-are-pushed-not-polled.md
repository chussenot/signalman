---
title: 0007 Changes are pushed to signalman, not polled from delivery tools
description: Recent deploys and configuration changes reach the triage through an authenticated push endpoint that any delivery tool can call, kept in a bounded in-memory window, rather than through clients for Argo CD, Flux, GitHub or Backstage deployment plugins.
status: accepted
date: 2026-09-21
decision-makers: [platform engineering]
consulted: [on-call leads]
informed: []
last_reviewed: 2026-09-21
tags: [decisions, changes, mttq]
---

# 0007 Changes are pushed to signalman, not polled from delivery tools

## Context and problem statement

`caused_by_change` is asked only when `alert.recent_changes` is non-empty, and in the webhook flow nothing filled it, so the judgment with the largest bearing on time to qualify never ran in production. Where should recent changes come from?

## Decision drivers

- Every organisation runs a different mix of delivery tools; the set changes
- Each polled upstream is a credential to hold, a contract to track and a failure mode to contain (decisions 0005, 0006)
- The tool that performed a change knows exactly when it finished; a poller estimates
- Correctness of the triage must not depend on the feed being present

## Considered options

1. Push: `POST /changes` with a bearer token, any tool posts when it deploys; signalman keeps a bounded in-memory window and matches by component
2. Poll the delivery tools directly (Argo CD, Flux, GitHub deployments API) at triage time
3. Poll through Backstage's plugins (the Argo CD or GitHub backend routes), reusing the Backstage credential
4. Read deploy annotations from Tsuga or another observability platform

## Decision outcome

Chosen option: push. It costs one bearer token that signalman itself owns, needs no knowledge of any delivery tool, and is wired with a notification template or a `curl` step at the source. The window lives in memory per replica: a change is context, and losing one on restart returns the flow to where it was before this decision, a question not asked.

### Consequences

- Good, because Argo CD notifications, Flux alerts, GitHub Actions and shell scripts all fit with configuration on their side and none on ours
- Good, because the change carries the exact completion time and a link, which the note shows
- Bad, because a change is only known if something posted it; adoption is per pipeline
- Bad, because the window is per replica and not persisted; a restart forgets it
- Neutral, because matching is by component name, which the posting side must spell as the alert labels do
- Neutral, because native adapters (Argo CD `Application`, GitLab webhooks) are translations of documented payloads into the same change, not clients: they read a body, they call nothing

### Confirmation

- `src/changes.rs` holds the window and matching; `src/serve.rs` routes `/changes` only when the token is configured
- `tests/webhook_server.rs` posts changes and asserts they reach the model state and the note
- No client for a delivery tool exists in the repository

## Pros and cons of the options

### Push

- Good, because zero upstream contracts and one secret
- Good, because it works for any tool, including ones that do not exist yet
- Bad, because completeness depends on every pipeline posting

### Poll delivery tools directly

- Good, because complete for the tools covered
- Bad, because one client, credential and contract per tool; Argo CD alone has several API shapes across versions
- Bad, because a poll at triage time adds latency and a failure mode to every alert

### Poll through Backstage plugins

- Good, because the Backstage credential is already held
- Bad, because plugin backend routes are internal to each plugin and version, not a published contract
- Bad, because the catalog then becomes a proxy for operational data it was not designed to serve

### Read observability deploy markers

- Bad, because decision 0005 already declined a direct observability client, and deploy markers there are themselves pushed by the same pipelines
