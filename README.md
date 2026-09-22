---
title: signalman
description: Alert triage that turns calibrated model judgments into routing decisions inside incident.io, grounded in the Backstage software catalog, written in Rust with a typed client for the TypeSafe System One API.
status: current
last_reviewed: 2026-09-22
tags: [overview]
---

# signalman

signalman triages alerts. It asks a decision model a fixed set of narrow questions about an alert, combines the answers in code, and writes the result back into incident.io as tags and incident attachments. It is a Rust service and CLI, built on a typed client for the [TypeSafe](https://typesafe.ai) System One API.

## Why it exists

Alert routing in most platforms is a pile of label matchers and regular expressions. They encode who owned what at the time they were written, they cannot read a description, and they cannot tell that a new alert is the same outage that already has an incident channel. The gaps are filled by the person on call, at the worst possible moment.

Large language models can read the description, but the usual approach, prompt and parse free text, gives an answer with no probability attached, drifts between runs, and hides the decision inside a paragraph. That is not a basis for paging someone at 03:00.

TypeSafe's model returns typed judgments with calibrated probabilities instead of text: one of a defined set, yes or no, a position on a scale. signalman uses that property to keep the two halves of the problem apart:

- The model answers semantic questions that code cannot: which team's component is failing, how many users are affected, whether a human must act, whether this alert is the same problem as an open incident.
- Code owns everything else: which questions to ask, the candidate incidents to offer, the thresholds at which a judgment becomes an action, and the actions themselves. Thresholds scale with the cost of being wrong. Paging needs a confident owner and a high impact; low confidence always goes to a person.

The Backstage software catalog, when present, is the ownership referential. The model chooses among the groups that actually own the alerting component and its neighbours, reads the component's record and its TechDocs runbook, and the owning group is told the outcome in the portal. Without a catalog a compiled-in team list is used.

incident.io remains the alert hub. signalman never creates incidents. It enriches alerts so that incident.io's own alert routes and escalation paths can act on them, and it attaches confident duplicates to the incident they belong to.

## What it does

- Receives incident.io `alert_created` webhooks, verifies the Svix signature, fetches the alert, the live incidents and the other alerts firing in the last half hour fresh from the API, judges, and writes back `ai-team-*`, `ai-impact-*`, `ai-action-*`, `ai-dup-*` and `ai-suspected-change` tags, plus an attachment for a confident duplicate.
- Accepts change events (deploys, configuration, flags) from Argo CD and GitLab in their native payloads, or from any delivery tool on `POST /changes`, and offers the ones that touched the alerting component in the last two hours to the model as `recent_changes`, so "did something change" is asked and answered.
- Leaves one qualification note on the alert: the decision, each judgment with its probability, the matched incident, the catalog component and owner with Backstage links, the TechDocs runbook page, the related firing alerts, and the time it took to qualify. Rewritten in place on a later pass, never stacked.
- Resolves the alerting service in the Backstage catalog: owner group and dependency neighbours become the owner options, the component record and TechDocs runbook join the state, and the owning group can be notified through the Notifications plugin.
- Triages an alert file from the CLI, optionally pulling live incidents as duplicate candidates, enriching from the catalog, and forwarding the enriched alert to an incident.io HTTP alert source.
- Emits one JSON document per triaged alert: the judgments with their probabilities, the decision, the thresholds that produced it, and what was written back. The schema is committed and a test fails when the code and the file drift apart, so a script or an agent can read the document without reading the source.
- Exposes the TypeSafe client as a library: a question returns a typed handle, and reading the answer through that handle yields a Rust enum, a probability, or a score. A response of the wrong shape is an error, not a misread number.

## What it does not do

- It does not create, edit or resolve incidents.
- It does not page anyone. It tags; incident.io routes.
- It does not generate text. No summaries, no explanations; only judgments a policy can threshold. The note is a fixed template filled with those judgments.
- It does not talk to the observability platform. Tsuga alerts reach it as incident.io alerts, through Tsuga's own integration.
- It has not yet run against a live TypeSafe, incident.io or Backstage instance. Every wire shape is asserted against the published documentation and OpenAPI specification, not observed traffic. See [the roadmap](docs/roadmap.md).

## Quick start

```sh
mise install            # toolchain, prek, bd
mise run setup          # git hooks
mise run check          # all quality gates
cp .env.example .env    # secrets only; never committed
cp examples/config/signalman.toml signalman.toml   # the deployment's shape; edit, then
mise run config:show    # see the effective configuration after file, env and flags
mise run serve          # webhook receiver on 127.0.0.1:8080
```

Configuration is layered, lowest to highest: built-in default, TOML file, environment variable, flag. Secrets are environment only. [Configuration](docs/configuration.md) lists every setting.

## Documentation

| Page | Contents |
|---|---|
| [Architecture](docs/architecture.md) | Components, the path of one alert, boundaries, failure containment |
| [C4 model](docs/c4/context.md) | Context, containers and components as C4 diagrams |
| [TypeSafe client](docs/typesafe-client.md) | Typed handles, `options!`, probabilities, defaults |
| [Laya as a model provider](docs/laya.md) | Running signalman against open weights, what it measured, why Jev stays the default |
| [Triage](docs/triage.md) | The questions, the policy, the outcome contract, how to tune it |
| [Evaluation harness](docs/evaluation.md) | Replay labelled alerts, grade judgments and decisions, tune without re-running inference |
| [incident.io integration](docs/incidentio.md) | Webhook flow, tags, alert routes, forwarding |
| [Change feed](docs/changes.md) | Pushing deploys and changes into the triage, wiring Argo CD, Flux and GitHub Actions |
| [Backstage bridge](docs/backstage.md) | Catalog ownership, TechDocs runbooks, notifications, registering signalman |
| [Configuration](docs/configuration.md) | The four layers, every setting, Kubernetes |
| [Operations](docs/operations.md) | Running, health, limits, failure modes |
| [Development](docs/development.md) | Tools, tasks, gates, layout, planning, agent harness |
| [Roadmap](docs/roadmap.md) | Known gaps and the beads backlog |
| [Decisions](docs/decisions/README.md) | Architecture decision records in MADR form |

## License

[MIT](LICENSE).
