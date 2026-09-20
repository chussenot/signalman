---
title: 0004 The catalog is the ownership source of truth
description: Owner candidates for the triage come from the Backstage software catalog when it is configured; the model chooses among them; the static team list is only a fallback.
status: accepted
date: 2026-09-20
decision-makers: [platform engineering]
consulted: []
informed: [on-call leads]
last_reviewed: 2026-09-20
tags: [decisions, backstage, triage]
---

# 0004 The catalog is the ownership source of truth

## Context and problem statement

The first version asked the model to pick an owner from a `Team` enum compiled into the binary. That list encodes one organisation at one moment, drifts, and knows nothing about which service an alert concerns or what that service depends on. Organisations that run Backstage maintain exactly this data. How should ownership enter the triage?

## Decision drivers

- Ownership changes should take effect without a code change or redeploy
- The model should judge with the registered owner in view, not guess it
- Alerts often fire on a symptom owned by one team and caused by a dependency owned by another
- Token cost grows with the number of options offered
- signalman must still work where no catalog exists

## Considered options

1. Catalog groups as owner candidates: the owner of the resolved component, the owners of its dependency neighbours and of components named in the alert text, plus a no-match option; the model chooses; static list as fallback
2. Catalog owner as the answer: resolve the component and route to its `spec.owner` without asking the model
3. Keep the static team list and maintain it by hand

## Decision outcome

Chosen option: "Catalog groups as owner candidates". The catalog supplies facts and candidates; the model still judges which candidate takes first response; `Policy` still decides what confidence is enough. Without a catalog, or when nothing matches, the static list applies.

### Consequences

- Good, because a catalog change reroutes the next alert with no code change
- Good, because the owner question becomes narrower and better grounded: the model is told the registered owner and asked whether this alert is an exception
- Good, because dependency neighbours put the likely cause and the blast radius in front of the model
- Bad, because every Backstage instance is a possible source of drift; types are lenient and missing entities degrade to the fallback rather than failing the triage
- Bad, because candidate lists cost tokens and are capped at 24; large organisations should filter by domain or system
- Bad, because routing quality now depends on catalog quality; unowned components and stale relations produce weaker candidates, which is a visible incentive to fix the catalog

### Confirmation

`OwnerCandidates` is the only input to the owner question; `TriageQuestions::for_alert_with` takes it and the enricher builds it. Tests assert the candidate set for a small catalog and the all-teams fallback. The owner instructions reference `alert.component.owner`.

## Pros and cons of the options

### Catalog groups as candidates, model chooses

- Good, because it uses the catalog for what it is authoritative about and the model for what it is good at
- Good, because `none_of_these` keeps unattributable alerts out of any team's queue
- Bad, because assembling candidates is up to five catalog calls per triage

### Catalog owner as the answer

- Good, because it is deterministic and free of tokens
- Bad, because it is wrong whenever the alert's component is the symptom and the cause is a dependency
- Bad, because it cannot express uncertainty, so nothing routes to a human

### Static team list

- Good, because it needs no external system
- Bad, because it drifts silently and cannot see the component graph
- Kept as the fallback

## More information

[Backstage bridge](../backstage.md), [Triage](../triage.md).
