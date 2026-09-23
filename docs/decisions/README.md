---
title: Decisions
description: Architecture decision records for signalman in MADR form, the index of accepted decisions, and how to add one.
status: current
last_reviewed: 2026-09-23
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
| [0007](0007-changes-are-pushed-not-polled.md) | Changes are pushed to signalman, not polled from delivery tools | accepted | `signalman-ufg.3` |
| [0008](0008-signalman-is-a-tool-for-agents.md) | signalman is a tool for agents, not an agent | accepted | `signalman-4gp.1` |
| [0009](0009-readiness-depends-on-the-upstreams.md) | Readiness depends on the upstreams | accepted | `signalman-m28.3` |

## Status notes

Records are not edited after acceptance, so a fact that has moved since a record was written is noted here rather than in the record. Each note is checked against the code, not against the record.

- 0006's confirmation counts four secret reads outside `src/config.rs`. There are now seven, in six files: the TypeSafe, incident.io and Backstage clients read their API key or token, the incident.io client also reads `INCIDENTIO_ALERT_SOURCE_TOKEN` for `triage --forward-to-incidentio`, the webhook verifier reads the signing secret, `src/changes/mod.rs` reads `SIGNALMAN_CHANGES_TOKEN`, and `src/mcp/http.rs` reads `SIGNALMAN_MCP_TOKEN`. Every one is a secret listed in [Configuration](../configuration.md#secrets); the rule the record states, that only `config.rs` reads a non-secret variable, holds again since the alert source id became the setting `incidentio.alert_source_config_id` (2026-09-23).
- 0007's confirmation names `src/changes.rs`. The module is now the directory `src/changes/`: `mod.rs` holds the window and matching, `argocd.rs` and `gitlab.rs` the native adapters the record describes as translations of documented payloads.
- 0008 says an MCP server "will serve" the same capabilities. It does: `signalman mcp` over stdio or Streamable HTTP, and `/mcp` on `serve` ([MCP server](../mcp.md)), with the write tool built as the record specifies, re-deriving every write from the outcome document, gated by `mcp.allow_write`.

## Writing a record

1. Copy [template.md](template.md) to `NNNN-short-title.md` with the next number, and drop its `llms: false` line: that key keeps only the template itself out of [llms.txt](../llms.txt).
2. Fill the frontmatter (`title`, `description`, `status`, `date`, `decision-makers`) and every section. Keep options concrete; a record with one option is a note, not a decision.
3. Set `status` to `proposed` until agreed, then `accepted`. A superseded record keeps its text and gains `status: superseded by 000N`.
4. Add the row above and, when a beads decision issue exists, its id.

Records are not edited after acceptance except to mark them superseded; a change is a new record.
