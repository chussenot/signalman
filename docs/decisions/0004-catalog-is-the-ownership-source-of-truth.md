---
title: 0004 The catalog is the ownership source of truth
description: Owner candidates for the triage come from the Backstage software catalog when it is configured; the static team list is only a fallback.
status: accepted
date: 2026-09-20
last_reviewed: 2026-09-20
tags: [decisions, backstage, triage]
---

# 0004 The catalog is the ownership source of truth

## Context

The first version asked the model to pick an owner from a `Team` enum compiled into the binary. That list encodes one organisation at one moment; it drifts, and it knows nothing about which service an alert concerns, what that service depends on, or what depends on it. Organisations that run Backstage already maintain exactly this data, with owners as `Group` entities and dependencies as relations.

## Decision

When Backstage is configured, the owner question's options are groups from the catalog: the owner of the resolved component, the owners of its neighbours in the dependency graph, and the owners of components named in the alert text, plus a no-match option. The component's catalog record and its TechDocs runbook are added to the state. The static list remains as the fallback when no catalog is configured or nothing matches.

The catalog supplies facts and candidates. The model still judges which candidate should take first response, because alerts often fire on a symptom owned by one team and caused by a dependency owned by another. `Policy` still decides what confidence is enough.

## Consequences

- Ownership changes in the catalog change routing on the next alert, with no code change and no redeploy.
- The question the model answers is narrower and better grounded: it is told the registered owner and asked whether this alert is an exception.
- Every Backstage instance becomes a possible source of drift: fields the code reads may be absent or renamed. Types are lenient and missing entities degrade to the fallback rather than failing the triage.
- Candidate lists cost tokens and are capped. Large organisations should filter groups by domain or system before offering them.
- signalman now depends on catalog quality. Unowned components and stale relations produce weaker candidates; that is a visible incentive to fix the catalog, not a reason to bypass it.
