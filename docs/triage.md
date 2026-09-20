---
title: Triage
description: The questions rustsafe asks about an alert, the policy that turns answers into a decision, and how to tune both.
status: current
last_reviewed: 2026-09-20
tags: [triage, typesafe, policy]
---

# Triage

`src/triage/` applies the TypeSafe documentation's recommended shape: keep deterministic work in code, ask narrow questions, ask them all at once, decide with thresholds that scale with risk.

## State

The `state` is `{ "alert": Alert }` where `Alert` carries source, title, description, labels, an optional runbook excerpt, recent changes, and open incidents as duplicate candidates. Only fields a question reads are included. Every field costs input tokens and dilutes attention.

## Questions

All questions are asked in one request. Each references the state by backticked path.

| Id | Primitive | Asked when | Read by the policy for |
|---|---|---|---|
| `owner` | Choice over `Team` | always | who receives the page or ticket |
| `impact` | Score over four levels | always | page versus ticket |
| `actionable` | Noul | always | suppression |
| `duplicate_of` | dynamic Choice over incident references plus `none` | at least one open incident | attaching to an existing incident |
| `caused_by_change` | Noul | at least one recent change | the `ai-suspected-change` tag |

The last two are speculative: asked up front because a second request would cost a round trip, and only read when their premise holds. They are omitted entirely when there are no candidates, because the model cannot choose an option it was not offered.

`Team` is defined with the `options!` macro in `src/triage/questions.rs`. Its variants and rubric text encode one organisation's ownership model and are the first thing to adapt. `none_of_these` exists so an unattributable alert is not forced onto a team.

`Impact::LEVELS` describes four concrete situations from no user-facing impact to full outage. Level text must stand alone; the model does not see the enum names.

## Policy

`decide` in `src/triage/policy.rs` is pure code over typed answers and returns one `Decision`:

| Decision | When |
|---|---|
| `Suppress` | `actionable` below `suppress_below` |
| `AttachToIncident` | dedup chose an incident with confidence at or above `attach_confidence` |
| `HumanTriage` | owner is `none_of_these`, or owner confidence below `human_below_confidence` |
| `Page` | impact at or above `page_at`; `confirm_owner` set when owner confidence is below `auto_route_confidence` |
| `Ticket` | otherwise |

Order encodes priority: suppression first, then dedup, then routing. Defaults:

| Threshold | Default |
|---|---|
| `suppress_below` | 0.25 |
| `attach_confidence` | 0.75 |
| `auto_route_confidence` | 0.70 |
| `human_below_confidence` | 0.40 |
| `page_at` | `Major` |
| `flag_change_above` | 0.65 |

These are conservative starting points taken from the documentation's three-band guidance. They are not tuned. Automatic paging on these values should be preceded by the evaluation work in the [roadmap](roadmap.md).

## Tuning

Raw answers are kept alongside the decision (`--json` on the CLI, `Outcome` in the flow), so a threshold can be changed and the decision recomputed without another model call. The intended loop:

1. Collect alerts with their expected team, impact and action.
2. Run them through `triage --json` and record the distributions and the versioned model id.
3. Pick thresholds per band from the observed confidence distributions, higher for actions that are expensive when wrong.
4. Pin the model version in `Policy` documentation and re-evaluate when moving to a new one.

## Testing

Policy tests build answers by round-tripping a fake API response through the real handles, so they exercise the same parsing as production. See `src/triage/policy.rs`.
