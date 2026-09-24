---
title: Documentation
description: Map of the signalman documentation, what each page is for, and the conventions the pages follow.
status: current
last_reviewed: 2026-09-24
tags: [index]
---

# Documentation

The [README](../README.md) says why signalman exists. These pages say how it works and how to work on it.

## By question

| You want to know | Read |
|---|---|
| What signalman talks to and why | [C4 context](c4/context.md) |
| What runs where | [C4 containers](c4/container.md), [Operations](operations.md) |
| How to see it run: traces, metrics, logs | [Observability](observability.md) |
| How the modules fit together | [C4 components](c4/component.md), [Architecture](architecture.md) |
| What the model is asked and how the answer becomes an action | [Triage](triage.md) |
| What one triage emits, and the JSON contract a script or an agent reads | [The outcome contract](triage.md#the-outcome-contract), [schema v1](schema/outcome.v1.json) |
| How well it decides, and how to tune it with numbers | [Evaluation harness](evaluation.md) |
| How the catalog changes ownership, context and runbooks | [Backstage bridge](backstage.md) |
| How webhooks are verified and what is written back | [incident.io integration](incidentio.md) |
| How deploys and configuration changes reach the triage | [Change feed](changes.md) |
| How an agent calls signalman's judgments | [MCP server](mcp.md) |
| Why the typed client is the `judgment` crate, separate from the triager, and how signalman uses it | [TypeSafe client](typesafe-client.md) |
| How to use the judgment core in another project | [TypeSafe client](typesafe-client.md), the crate README |
| Whether an open-weights model can replace Jev | [Laya as a model provider](laya.md) |
| Whether the judgment crate works against Laya, and what the benchmark measured | [judgment against Laya typed-decisions](judgment-laya-typed-decisions.md) |
| Which variable to set | [Configuration](configuration.md) |
| How to contribute | [Development](development.md) |
| What is unverified or missing | [Roadmap](roadmap.md) |
| Why a constraint exists | [Decisions](decisions/README.md) |
| Everything, as an agent or a model reads it | [llms.txt](llms.txt), the index; [llms-full.txt](llms-full.txt), every page in one file |

## Conventions

- Every page starts with YAML frontmatter. `title` and `description` are required and checked by `scripts/check-frontmatter.sh`; `status` is `current`, `draft` or `experiment` for a page (`experiment` marks a page about something run once and kept for its measurements, such as [Laya](laya.md); it is not a supported path and the page says so) and a [MADR](https://adr.github.io/madr/) status (`proposed`, `accepted`, `superseded by 000N`) for a decision record; `last_reviewed` is the date someone last confirmed the page against the code; `tags` help search.
- `llms.txt` and `llms-full.txt` are generated from the `mkdocs.yml` nav and the frontmatter by `scripts/gen-llms-txt.sh` (`mise run docs:llms`), in nav order; a page opts out with `llms: false` (the decision template does). `mise run check` fails when they are stale. Never edit them by hand: change the page or the nav.
- Diagrams are Mermaid, kept next to the prose they explain. GitHub renders them natively; TechDocs needs the Mermaid addon ([Development](development.md#techdocs)).
- Decision records follow [MADR](https://adr.github.io/madr/) ([Decisions](decisions/README.md)).
- Pages describe the code as it is. A claim that depends on something unverified says so where the claim is made.
