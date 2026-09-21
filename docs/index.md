---
title: Documentation
description: Map of the signalman documentation, what each page is for, and the conventions the pages follow.
status: current
last_reviewed: 2026-09-20
tags: [index]
---

# Documentation

The [README](../README.md) says why signalman exists. These pages say how it works and how to work on it.

## By question

| You want to know | Read |
|---|---|
| What signalman talks to and why | [C4 context](c4/context.md) |
| What runs where | [C4 containers](c4/container.md), [Operations](operations.md) |
| How the modules fit together | [C4 components](c4/component.md), [Architecture](architecture.md) |
| What the model is asked and how the answer becomes an action | [Triage](triage.md) |
| How the catalog changes ownership, context and runbooks | [Backstage bridge](backstage.md) |
| How webhooks are verified and what is written back | [incident.io integration](incidentio.md) |
| How the typed client works and its defaults | [TypeSafe client](typesafe-client.md) |
| Whether an open-weights model can replace Jev | [Laya as a model provider](laya.md) |
| Which variable to set | [Configuration](configuration.md) |
| How to contribute | [Development](development.md) |
| What is unverified or missing | [Roadmap](roadmap.md) |
| Why a constraint exists | [Decisions](decisions/README.md) |

## Conventions

- Every page starts with YAML frontmatter. `title` and `description` are required and checked by `scripts/check-frontmatter.sh`; `status` is `current` or `draft`; `last_reviewed` is the date someone last confirmed the page against the code; `tags` help search.
- Diagrams are Mermaid, kept next to the prose they explain. GitHub renders them natively; TechDocs needs the Mermaid addon ([Development](development.md#techdocs)).
- Decision records follow [MADR](https://adr.github.io/madr/) ([Decisions](decisions/README.md)).
- Pages describe the code as it is. A claim that depends on something unverified says so where the claim is made.
