---
title: Documentation
description: Map of the signalman documentation, what each page is for, and the conventions the pages follow.
status: current
last_reviewed: 2026-10-03
tags: [index]
---

# Documentation

The [README](../README.md) says why signalman exists. These pages say how it works and how to work on it. The `judgment` crate, the typed TypeSafe client signalman is built on, documents itself in [its own repository](https://github.com/chussenot/judgment/blob/main/docs/index.md) ([decision 0011](decisions/0011-documentation-lives-with-its-concern.md), [decision 0012](decisions/0012-the-judgment-crate-moves-to-its-own-repository.md)).

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
| How the crate works, what it guarantees, what real servers did with it, how to use it in another project | the crate's own documentation: [README](https://github.com/chussenot/judgment/blob/main/README.md), [index](https://github.com/chussenot/judgment/blob/main/docs/index.md) |
| Whether an open-weights model can replace Jev | [Laya as a model provider](laya.md) |
| How the crate moves to its own repository with its history, and what each repository changes then | [Extracting the judgment crate](judgment-extraction.md) |
| What the open Jev reproductions and the Decision Index mean for provider choice, self-hosting, question design and thresholds | [Open System One models](research/open-system-one-models.md) |
| What a catalogue of 249 typed decisions over the same wire does differently, and what signalman took from it: the state-as-data rule, evaluation provenance, splits, fingerprints and intervals | [Decision recipes](research/decision-recipes.md) |
| Where a Rust macro would remove repetition, where it would hide it, and what to do instead | [Rust macros](research/rust-macros.md) |
| Which variable to set | [Configuration](configuration.md) |
| How to contribute | [Development](development.md) |
| What is unverified or missing | [Roadmap](roadmap.md) |
| Why a constraint exists | [Decisions](decisions/README.md) |
| Everything, as an agent or a model reads it | [llms.txt](llms.txt), the index; [llms-full.txt](llms-full.txt), every page in one file |

## Conventions

- Every page starts with YAML frontmatter. `title` and `description` are required and checked by `scripts/check-frontmatter.sh`; `status` is `current`, `draft` or `experiment` for a page (`experiment` marks a page about something run once and kept for its measurements, such as [Laya](laya.md); it is not a supported path and the page says so) and a [MADR](https://adr.github.io/madr/) status (`proposed`, `accepted`, `superseded by 000N`) for a decision record; `last_reviewed` is the date someone last confirmed the page against the code; `tags` help search.
- `llms.txt` and `llms-full.txt` are generated from the `mkdocs.yml` nav and the frontmatter by `scripts/gen-llms-txt.sh` (`mise run docs:llms`), in nav order; a page opts out with `llms: false` (the decision template does). `mise run check` fails when they are stale. Never edit them by hand: change the page or the nav.
- Diagrams are Mermaid, kept next to the prose they explain. GitHub renders them natively; TechDocs needs the Mermaid addon ([Development](development.md#documentation)).
- Decision records follow [MADR](https://adr.github.io/madr/) ([Decisions](decisions/README.md)).
- A page belongs here when it is about signalman. A page that would still be true, and still be needed, if signalman did not exist belongs to the `judgment` crate, in [its repository](https://github.com/chussenot/judgment) ([decision 0011](decisions/0011-documentation-lives-with-its-concern.md), [decision 0012](decisions/0012-the-judgment-crate-moves-to-its-own-repository.md)). A page that informs both, such as [Open System One models](research/open-system-one-models.md), stays here and says what it means for the crate in a section of its own.
- Links between pages here are relative. A link to the crate's documentation is an absolute GitHub URL, because it leaves this documentation set.
- Pages describe the code as it is. A claim that depends on something unverified says so where the claim is made.
