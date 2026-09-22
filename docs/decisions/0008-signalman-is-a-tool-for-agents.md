---
title: 0008 signalman is a tool for agents, not an agent
description: Expose signalman's typed qualification to external agents through a versioned JSON contract and, later, an MCP server; embed no generative-model loop in the triage path.
status: accepted
date: 2026-09-22
decision-makers: [platform engineering]
consulted: []
informed: [on-call leads]
last_reviewed: 2026-09-22
tags: [decisions, agents, mcp]
---

# 0008 signalman is a tool for agents, not an agent

## Context and problem statement

Responders increasingly work through assistants: a coding agent in the terminal, an assistant in the chat tool, the incident platform's own helper. The question keeps returning in a new form: should signalman gain agent capabilities, that is, investigate an alert on its own, read dashboards and runbooks, and eventually act? The concrete candidate was [Cersei](https://github.com/pacifio/cersei), a Rust SDK for coding agents (MIT, 0.2.6 on crates.io), evaluated on 2026-09-22: a generative-model agentic loop over Anthropic, OpenAI, Gemini and Vertex providers, with sub-agents, file, shell and web tools, memory tiers, skills, an MCP client and a serialisable workflow engine.

## Decision drivers

- Decisions [0001](0001-incidentio-remains-the-alert-hub.md) and [0002](0002-calibrated-judgments-over-generated-text.md) hold: writes to incident.io stay bounded, no model generates text, and every decision is traceable to a probability and a threshold
- Responders need signalman's judgments inside the assistants they already use, not one more assistant with its own conversation
- signalman runs per alert under an autoscaler; its dependency tree and failure modes must stay those of a small service
- Everything an agent can do through signalman must be something an operator can do through the CLI, so behaviour stays testable without a model in the loop

## Considered options

1. Embed an agent SDK (Cersei) so the triage path runs an agentic loop with tools
2. Expose typed capabilities to external agents: a versioned JSON contract for the outcome, then an MCP server, read-only first and a gated write tool later
3. A hand-written investigation loop without an SDK: fetch the runbook and dashboards, have a generative model summarise a likely cause into the note
4. Do nothing; agents drive the CLI and parse its text

## Decision outcome

Chosen option: expose typed capabilities to external agents. signalman is a tool that agents call; it is not itself an agent. The outcome of a triage is a versioned JSON document ([schema v1](../schema/outcome.v1.json), described in [Triage](../triage.md#the-outcome-contract)) emitted by every `--json` path and the receiver's log line. An MCP server (`signalman-4gp.5`) will serve the same capabilities the CLI subcommands already expose, from the same functions, read-only first. A write tool, when added, takes the outcome document back as its input and re-derives tags, note and attachment from it, so an agent cannot invent a write.

A generative model may appear in offline tooling outside the binary, such as labelling alert history for the evaluation harness, and never in the triage path and never with write access to incident.io.

### Consequences

- Good, because the deterministic core and decisions 0001 and 0002 stand; nothing in signalman chooses actions from prose
- Good, because each capability has one implementation, shared by the CLI subcommand and the MCP tool, and one test suite against mocks
- Good, because agents receive calibrated probabilities and links rather than a paragraph to trust
- Good, because the contract replaces a `--json` output whose field names followed a Rust struct and could change with a rename
- Bad, because signalman cannot answer an open-ended question about an alert; the agent that calls it owns the investigation and the conversation
- Bad, because an MCP server is a new surface: over stdio it is trusted by the process boundary, over HTTP it needs the bearer token and the same rate limits as the receiver
- Neutral, because the one investigation step with measurable value for time to qualify, choosing the runbook section that applies, is delivered as a typed Choice question (`signalman-3l7.8`), not as an agent

### Confirmation

- `Cargo.toml` carries no generative-model SDK: no Anthropic, OpenAI, Gemini or agent-framework crate
- `docs/schema/outcome.v1.json` is generated from the wire types and a test fails when the emitted shape drifts from the committed file
- When the MCP server exists, its tool list is built from the same functions as the CLI subcommands, and a test asserts the write tool is absent unless `mcp.allow_write` is true

### Revisit criteria

This record is reopened only when all of the following hold. They are tracked as `signalman-4gp.4`, deferred.

1. A responder need that cannot be met by deterministic retrieval, a typed question and an external agent calling signalman
2. Live verification (`signalman-b11`) complete and the harness showing calibrated decisions on real history (`signalman-ufg.6`), because acting compounds every qualification error
3. A human approval channel in incident.io that signalman can wait on, with the approver, the action and the target written to the timeline before anything runs

## Pros and cons of the options

### Embed an agent SDK (Cersei)

- Good, because the loop, tool dispatch, permissions and hooks exist and are tested; Cersei's `Tool`, `PermissionPolicy` and `Hook` traits are a reasonable shape for an allow-listed action surface
- Bad, because an agentic loop lets the model choose actions and write prose, which decisions 0001 and 0002 exclude
- Bad, because its tools crate pulls five tree-sitter grammars, tantivy, a process library, an LSP client and an embeddings crate, and the tree uses reqwest 0.12 and chrono where signalman uses reqwest 0.13 and jiff; the binary would carry two HTTP stacks
- Bad, because it is a 0.2.x SDK with one maintainer, about 700 downloads on crates.io, no declared MSRV and in-code notes of removed APIs; its abstractions (working directory, session, `CLAUDE.md` memory) are shaped for a coding CLI
- Bad, because it offers an MCP client, not a server; the direction responders need is the reverse

### Expose typed capabilities to external agents

- Good, because it keeps every existing rule and adds no model
- Good, because the same contract serves scripts, log pipelines and agents
- Bad, because value depends on the agents the organisation runs; without one, the MCP server idles
- Bad, because a second transport for the same capabilities is more surface to document and secure

### Hand-written investigation loop with a generative model

- Good, because it targets the one thing responders ask for, a likely cause
- Bad, because a generated cause in the note is prose the responder has to verify at the worst moment; decision 0002 declined exactly this
- Bad, because it needs a generative-model provider, its key and its cost in the triage path

### Do nothing

- Good, because nothing changes
- Bad, because agents then parse human-oriented text, and a wording change breaks them silently
- Bad, because the ad hoc `--json` shape was already an unversioned contract by accident

## More information

- Beads: `signalman-4gp` (epic), `signalman-4gp.1` (this record), `signalman-4gp.2` (contract), `signalman-4gp.5` and `signalman-4gp.6` (MCP), `signalman-4gp.4` (remediation, deferred)
- [Cersei repository](https://github.com/pacifio/cersei) and [documentation](https://cersei.pacifio.dev/docs)
- [Model Context Protocol](https://modelcontextprotocol.io) and its [Rust SDK](https://github.com/modelcontextprotocol/rust-sdk)
- Related records: [0001](0001-incidentio-remains-the-alert-hub.md), [0002](0002-calibrated-judgments-over-generated-text.md)
