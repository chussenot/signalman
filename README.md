---
title: rustsafe
description: Alert triage that turns calibrated model judgments into routing decisions inside incident.io, written in Rust with a typed client for the TypeSafe System One API.
status: current
last_reviewed: 2026-09-20
tags: [overview]
---

# rustsafe

rustsafe triages alerts. It asks a decision model a fixed set of narrow questions about an alert, combines the answers in code, and writes the result back into incident.io as tags and incident attachments. It is a Rust service and CLI, built on a typed client for the [TypeSafe](https://typesafe.ai) System One API.

## Why it exists

Alert routing in most platforms is a pile of label matchers and regular expressions. They encode who owned what at the time they were written, they cannot read a description, and they cannot tell that a new alert is the same outage that already has an incident channel. The gaps are filled by the person on call, at the worst possible moment.

Large language models can read the description, but the usual approach, prompt and parse free text, gives an answer with no probability attached, drifts between runs, and hides the decision inside a paragraph. That is not a basis for paging someone at 03:00.

TypeSafe's model returns typed judgments with calibrated probabilities instead of text: one of a defined set, yes or no, a position on a scale. rustsafe uses that property to keep the two halves of the problem apart:

- The model answers semantic questions that code cannot: which team's component is failing, how many users are affected, whether a human must act, whether this alert is the same problem as an open incident.
- Code owns everything else: which questions to ask, the candidate incidents to offer, the thresholds at which a judgment becomes an action, and the actions themselves. Thresholds scale with the cost of being wrong. Paging needs a confident owner and a high impact; low confidence always goes to a person.

incident.io remains the alert hub. rustsafe never creates incidents. It enriches alerts so that incident.io's own alert routes and escalation paths can act on them, and it attaches confident duplicates to the incident they belong to.

## What it does

- Receives incident.io `alert_created` webhooks, verifies the Svix signature, fetches the alert and the live incidents fresh from the API, judges, and writes back `ai-team-*`, `ai-impact-*`, `ai-action-*`, `ai-dup-*` and `ai-suspected-change` tags, plus an attachment for a confident duplicate.
- Triages an alert file from the CLI, optionally pulling live incidents as duplicate candidates and forwarding the enriched alert to an incident.io HTTP alert source.
- Exposes the TypeSafe client as a library: a question returns a typed handle, and reading the answer through that handle yields a Rust enum, a probability, or a score. A response of the wrong shape is an error, not a misread number.

## What it does not do

- It does not create, edit or resolve incidents.
- It does not page anyone. It tags; incident.io routes.
- It does not generate text. No summaries, no explanations; only judgments a policy can threshold.
- It has not yet run against a live TypeSafe or incident.io account. Every wire shape is asserted against the published documentation and OpenAPI specification, not observed traffic. See [the roadmap](docs/roadmap.md).

## Quick start

```sh
mise install            # toolchain, prek, bd
mise run setup          # git hooks
mise run check          # all quality gates
cp .env.example .env    # fill in keys; never committed
mise run serve          # webhook receiver on 127.0.0.1:8080
```

## Documentation

| Page | Contents |
|---|---|
| [Architecture](docs/architecture.md) | Components, data flow, boundaries |
| [TypeSafe client](docs/typesafe-client.md) | Typed handles, `options!`, probabilities, defaults |
| [Triage](docs/triage.md) | The questions, the policy, how to tune it |
| [incident.io integration](docs/incidentio.md) | Webhook flow, tags, alert routes, forwarding |
| [Configuration](docs/configuration.md) | Environment variables and their scopes |
| [Operations](docs/operations.md) | Running, health, limits, failure modes |
| [Development](docs/development.md) | Tools, tasks, gates, layout, planning, agent harness |
| [Roadmap](docs/roadmap.md) | Known gaps and the beads backlog |
| [Decisions](docs/decisions/README.md) | Architecture decision records |

## License

MIT or Apache-2.0, at your option.
