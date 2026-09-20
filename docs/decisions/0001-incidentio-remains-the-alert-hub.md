---
title: 0001 incident.io remains the alert hub
description: signalman enriches alerts with tags and attachments and never creates incidents, so incident.io alert routes keep ownership of escalation.
status: accepted
date: 2026-09-20
decision-makers: [platform engineering]
consulted: []
informed: [on-call leads]
last_reviewed: 2026-09-20
tags: [decisions, incidentio]
---

# 0001 incident.io remains the alert hub

## Context and problem statement

incident.io already receives alerts from every monitoring source, groups them, and decides through alert routes whether to open an incident and whom to escalate to. signalman adds a judgment to each alert. Where should that judgment take effect: by signalman acting on incident.io's behalf (creating and escalating incidents), or by handing incident.io better inputs and letting its routes act?

## Decision drivers

- One place where operators change escalation behaviour
- A wrong model judgment must be cheap and visible to undo
- incident.io limits incident creation to 10 per hour per API key when a chat channel is created
- Responders already live in incident.io; a second workflow tool would compete with it

## Considered options

1. signalman writes alert tags and alert-to-incident attachments; alert routes decide
2. signalman creates and escalates incidents directly through the API
3. signalman runs as an incident.io alert source, posting enriched alert events that routes consume

## Decision outcome

Chosen option: "Tags and attachments; alert routes decide", with option 3 kept as the CLI's forwarding mode for alerts that do not yet reach incident.io. signalman never creates, edits or resolves incidents.

### Consequences

- Good, because escalation stays in one routing model that operators already know
- Good, because a wrong judgment is a wrong tag, reversible in the UI, not a paged responder
- Good, because the incident creation limit is never approached
- Bad, because anything signalman wants incident.io to do must be expressible as an alert route condition on tags
- Bad, because tags arrive a few seconds after the alert; routes must evaluate on update or wait

### Confirmation

The incident.io client exposes no method that creates, edits or resolves incidents. The `contract-reviewer` agent and CLAUDE.md state the rule. Beads decision `signalman-p2w`.

## Pros and cons of the options

### Tags and attachments; alert routes decide

- Good, because it composes with incident.io's own grouping, escalation and on-call schedules
- Good, because dry-run and review are trivial: read the tags
- Bad, because expressiveness is bounded by what routes can match on

### Direct incident creation

- Good, because signalman could set severity, roles and summary in one call
- Bad, because a false positive pages a person and creates a channel before anyone reviews it
- Bad, because the 10-per-hour creation limit turns an alert storm into silent drops
- Bad, because two systems now own escalation logic

### Alert source forwarding

- Good, because judgments become alert attributes, the richest form routes can read
- Bad, because signalman must sit in the ingestion path, and alerts already in incident.io would be duplicated
- Kept as the CLI option for sources that do not yet feed incident.io

## More information

[incident.io integration](../incidentio.md). Revisit only if alert routes prove unable to express a decision signalman needs.
