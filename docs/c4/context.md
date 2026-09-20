---
title: C4 level 1, system context
description: signalman in its environment, the people and systems it exchanges information with, and what flows between them.
status: current
last_reviewed: 2026-09-20
tags: [architecture, c4]
---

# C4 level 1: system context

The context diagram answers one question: what does signalman talk to, and why. Everything inside the boundary is described at the next level in [containers](container.md).

```mermaid
C4Context
    title System context for signalman
    Person(oncall, "On-call engineer", "Receives pages and tickets through incident.io; reads notifications in Backstage")
    Person(operator, "Platform engineer", "Runs signalman, tunes the policy, maintains the catalog")
    System(signalman, "signalman", "Triages alerts: asks a decision model narrow questions, decides in code, writes the result back")
    System_Ext(monitoring, "Monitoring", "Datadog, Alertmanager and other alert sources")
    System_Ext(incidentio, "incident.io", "Alert and incident hub: alerts, alert routes, incidents, escalations")
    System_Ext(typesafe, "TypeSafe System One API", "Calibrated typed judgments (Jev)")
    System_Ext(backstage, "Backstage", "Software catalog, TechDocs, Notifications")
    Rel(monitoring, incidentio, "sends alerts")
    Rel(incidentio, signalman, "alert created webhook", "HTTPS, Svix-signed")
    Rel(signalman, incidentio, "reads alert and live incidents; writes tags and attachments", "REST, bearer token")
    Rel(signalman, typesafe, "one fan-out request per alert", "REST, bearer token")
    Rel(signalman, backstage, "reads catalog and TechDocs; sends notifications", "REST, static token")
    Rel(incidentio, oncall, "escalates by alert route")
    Rel(backstage, oncall, "shows the decision as a notification")
    Rel(operator, signalman, "configures, runs the CLI")
    UpdateLayoutConfig($c4ShapeInRow="3", $c4BoundaryInRow="1")
```

## Relationships

| From | To | What flows | Protocol |
|---|---|---|---|
| Monitoring | incident.io | Alerts | Vendor integrations, unchanged by signalman |
| incident.io | signalman | `public_alert.alert_created_v1` webhook | HTTPS POST, Svix HMAC-SHA256 signature |
| signalman | incident.io | Alert by id; incidents in triage, live and paused; add tags; attach alert to incident | REST v2, API key |
| signalman | TypeSafe | One request: state plus every question; answers with probabilities | REST v1, API key |
| signalman | Backstage | Entities by name, query and refs; TechDocs search index; notification to a group | REST, static external-access token |
| incident.io | On-call engineer | Escalation decided by alert routes reading the `ai-*` tags | incident.io native |
| Backstage | On-call engineer | Notification with decision, severity and alert link | Notifications plugin |

## What is deliberately outside

signalman does not talk to the monitoring systems, does not page anyone directly, and does not create incidents. Those are incident.io's roles ([ADR 0001](../decisions/0001-incidentio-remains-the-alert-hub.md)). It holds no database; the only state is an in-memory set of recent webhook ids.
