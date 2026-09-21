---
title: Decisions
description: Architecture decision records for signalman in MADR form, the index of accepted decisions, and how to add one.
status: current
last_reviewed: 2026-09-20
tags: [decisions, adr, madr]
---

# Decisions

Each record captures one architecturally significant decision: the problem, the options that were weighed, the choice, and what it costs. Records follow [MADR](https://adr.github.io/madr/), the Markdown Any Decision Record format, so the structure is the same across records and readers can compare options rather than reconstruct them.

| Id | Title | Status | Beads |
|---|---|---|---|
| [0001](0001-incidentio-remains-the-alert-hub.md) | incident.io remains the alert hub | accepted | `signalman-p2w` |
| [0002](0002-calibrated-judgments-over-generated-text.md) | Calibrated judgments over generated text | accepted | |
| [0003](0003-typed-handles-between-questions-and-answers.md) | Typed handles between questions and answers | accepted | |
| [0004](0004-catalog-is-the-ownership-source-of-truth.md) | The catalog is the ownership source of truth | accepted | |
| [0005](0005-observability-signals-enter-through-incidentio.md) | Observability signals enter through incident.io | accepted | `signalman-3l7.4` |
| [0006](0006-layered-configuration.md) | Layered configuration with secrets outside the file | accepted | `signalman-071.4` |

## Writing a record

1. Copy [template.md](template.md) to `NNNN-short-title.md` with the next number.
2. Fill the frontmatter (`title`, `description`, `status`, `date`, `decision-makers`) and every section. Keep options concrete; a record with one option is a note, not a decision.
3. Set `status` to `proposed` until agreed, then `accepted`. A superseded record keeps its text and gains `status: superseded by 000N`.
4. Add the row above and, when a beads decision issue exists, its id.

Records are not edited after acceptance except to mark them superseded; a change is a new record.
