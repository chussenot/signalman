---
title: Triage
description: The state signalman builds for an alert, the questions it asks in one request, the policy that turns the answers into a decision, and how to tune it.
status: current
last_reviewed: 2026-09-20
tags: [triage, typesafe, policy]
---

# Triage

Triage is one request to the model and one pass through a policy. The request asks every question the policy might need, including questions whose answers may go unused. The policy reads the answers in a fixed order of precedence and returns exactly one decision. Nothing in the policy calls the model, so thresholds can change without another request.

## State

The `state` is `{ "alert": Alert }`. Every field either answers a question or is left out; each one costs input tokens and dilutes attention.

| Field | Source | Read by |
|---|---|---|
| `source`, `title`, `description`, `labels` | the alert | every question |
| `runbook` | the alert, or TechDocs when Backstage is configured | owner, actionable |
| `open_incidents` | live incidents from incident.io, capped at 40 | `duplicate_of` |
| `recent_changes` | the alert file today; deploy sources are on the roadmap | `caused_by_change` |
| `component` | the Backstage catalog: owner, lifecycle, system, dependencies, dependents, tags, links | owner, impact |
| `related_alerts` | alerts firing in incident.io in the recent window (default 30 minutes, at most 20): title, age in minutes, component label | impact |

`related_alerts` is the blast radius as the hub sees it at that moment. When it is non-empty the impact question is told that several alerts on the same component, or on the component's dependents, indicate broader impact than the alert alone shows, and that unrelated ones do not raise it. The list is also written into the [qualification note](incidentio.md#the-qualification-note), so the responder sees the same context the model did.

## Which questions are asked

Speculative questions are asked in the same request when their premise can hold, and never when it cannot: the model cannot choose an option it was not offered.

```mermaid
flowchart TD
    A[alert state] --> O[owner: dynamic Choice<br/>over owner candidates + none_of_these]
    A --> I[impact: Score over 4 levels]
    A --> N[actionable: Noul]
    A --> D{open_incidents<br/>non-empty?}
    D -->|yes| DQ[duplicate_of: dynamic Choice<br/>over incident refs + none]
    D -->|no| DS[not asked]
    A --> C{recent_changes<br/>non-empty?}
    C -->|yes| CQ[caused_by_change: Noul]
    C -->|no| CS[not asked]
    O & I & N & DQ & CQ --> R[one POST /v1/systemone]
```

| Id | Primitive | Rubric | Policy reads it for |
|---|---|---|---|
| `owner` | Choice | owner candidates: catalog groups, or the static `Team` list | who receives the page or ticket |
| `impact` | Score | `Impact::LEVELS`, four concrete situations from no user impact to full outage | page versus ticket |
| `actionable` | Noul | yes: failing, at risk or violating policy and will not self-resolve; no: informational, test, recovered, blip | suppression |
| `duplicate_of` | Choice | open incident references plus `none` | attaching to an existing incident |
| `caused_by_change` | Noul | is one of the listed changes a plausible direct cause | the `ai-suspected-change` tag |

Every question references the state by backticked path (`alert.title`, `alert.component.owner`). The owner question's instructions change when a catalog component is present: they name `alert.component.owner` as the registered owner and say when to deviate.

### Owner candidates

Owner candidates are data, not a type. With Backstage configured they are catalog groups assembled by the [enricher](backstage.md#owner-candidates), each described by display name, description and owned components. Without a catalog, `Team` in `src/triage/questions.rs`, defined with the `options!` macro, supplies a static list to adapt to your organisation. `none_of_these` is always the last option so an unattributable alert is never forced onto a team. The chosen key becomes the `ai-team-<key>` tag and, for catalog groups, the notification recipient.

## The decision

`decide` in `src/triage/policy.rs` is pure code over typed answers.

```mermaid
flowchart TD
    S[typed answers] --> A{actionable<br/>< suppress_below?}
    A -->|yes| SUP[Suppress]
    A -->|no| D{duplicate_of chosen ≠ none<br/>and confidence ≥ attach_confidence?}
    D -->|yes| ATT[AttachToIncident]
    D -->|no| O{owner is none_of_these<br/>or confidence < human_below_confidence?}
    O -->|yes| HUM[HumanTriage<br/>best guess = highest-probability named candidate]
    O -->|no| IMP{impact ≥ page_at?}
    IMP -->|yes| PAGE[Page<br/>confirm_owner if confidence < auto_route_confidence]
    IMP -->|no| TIC[Ticket<br/>confirm_owner if confidence < auto_route_confidence]
    PAGE & TIC --> CH{caused_by_change<br/>> flag_change_above?}
    CH -->|yes| F[suspected_change = true]
```

Order is priority. Suppression wins over everything: a non-actionable alert is dropped even if it looks like a duplicate. Deduplication wins over routing: attaching to the incident that already has responders beats paging a second team. Only then does ownership matter, and only a confident owner gets an automatic route.

| Threshold | Default | Meaning |
|---|---|---|
| `suppress_below` | 0.25 | below this probability of being actionable, suppress |
| `attach_confidence` | 0.75 | dedup confidence needed to attach |
| `human_below_confidence` | 0.40 | owner confidence below which a person triages |
| `auto_route_confidence` | 0.70 | owner confidence below which the route is marked for confirmation |
| `page_at` | `Major` | impact level at or above which the owner is paged rather than ticketed |
| `flag_change_above` | 0.65 | probability above which a recent change is flagged as suspected cause |

These are conservative starting points from the TypeSafe documentation's three-band guidance and have not been tuned on real alerts. Automatic paging on these values should wait for the evaluation work in the [roadmap](roadmap.md).

## Confidence is not probability

A Choice answer carries both the probability of each option and a confidence, which summarises how concentrated the distribution is. The policy thresholds on confidence for routing and dedup because a spread distribution means the model saw several plausible answers, whatever the top one was. A Noul carries only a probability; the actionable threshold reads that directly.

## Tuning

Raw answers travel with every decision (`--json` on the CLI, `Outcome` in the flow). The loop:

1. Collect alerts with the expected team, impact and action.
2. Run them through `triage --json` and record the distributions and the versioned model id.
3. Pick thresholds per band from the observed confidence distributions, higher for actions that are expensive when wrong.
4. Pin the model version alongside the thresholds and re-evaluate before moving to a new one.

## Testing

Policy tests build answers by round-tripping a fake API response through the real handles (`src/triage/policy.rs`), so they exercise the same parsing as production. Question tests assert which questions exist for a given state and what the owner criteria contain.
