---
title: Documentation
description: Map of the signalman documentation set and the conventions its pages follow.
status: current
last_reviewed: 2026-09-20
tags: [index]
---

# Documentation

Start with the [README](../README.md) for why the application exists. These pages cover how it works and how to work on it.

| Page | Read it when |
|---|---|
| [Architecture](architecture.md) | You need the components and the data flow in one place |
| [TypeSafe client](typesafe-client.md) | You are calling the model or adding a question type |
| [Triage](triage.md) | You are changing a question, a team, a level or a threshold |
| [incident.io integration](incidentio.md) | You are connecting an account or changing what is written back |
| [Backstage bridge](backstage.md) | You are wiring the catalog, TechDocs or notifications, or registering signalman |
| [Configuration](configuration.md) | You need an environment variable |
| [Operations](operations.md) | You are running it |
| [Development](development.md) | You are contributing |
| [Roadmap](roadmap.md) | You want to know what is unverified or missing |
| [Decisions](decisions/README.md) | You want the reasoning behind a constraint |

## Conventions

Every page starts with YAML frontmatter. `title` and `description` are required and checked by `scripts/check-frontmatter.sh`. `status` is `current` or `draft`. `last_reviewed` is the date someone last confirmed the page against the code. `tags` help search.

Pages describe the code as it is. Anything not verified against a live system says so on the page where the claim is made.
