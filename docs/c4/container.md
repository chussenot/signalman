---
title: C4 level 2, containers
description: The deployable units of signalman, the one process with a receiver and a CLI, and the external services each one uses.
status: current
last_reviewed: 2026-09-20
tags: [architecture, c4]
---

# C4 level 2: containers

signalman ships as one binary. Running it as `signalman serve` gives the webhook receiver; every other subcommand is the CLI. Both use the same library, so this level has two containers sharing one code path.

```mermaid
C4Container
    title Containers of signalman
    Person(oncall, "On-call engineer")
    Person(operator, "Platform engineer")
    System_Boundary(sm, "signalman") {
        Container(serve, "Webhook receiver", "Rust, axum, tokio", "POST /webhooks/incidentio: verifies, deduplicates, acknowledges, runs the triage in the background. GET /healthz.")
        Container(cli, "CLI", "Rust, clap", "triage, incidentio, backstage and models commands for operators and evaluation")
        Container(lib, "Triage library", "Rust crate", "Typed TypeSafe client, triage questions and policy, incident.io and Backstage bridges, shared retry loop")
    }
    System_Ext(incidentio, "incident.io", "Alerts, incidents, alert routes")
    System_Ext(typesafe, "TypeSafe System One API", "Jev")
    System_Ext(backstage, "Backstage", "Catalog, TechDocs, Notifications")
    Rel(incidentio, serve, "alert created webhook", "HTTPS")
    Rel(operator, cli, "runs", "shell")
    Rel(serve, lib, "calls Triager")
    Rel(cli, lib, "calls Triager, Enricher, clients")
    Rel(lib, incidentio, "REST v2", "HTTPS, API key")
    Rel(lib, typesafe, "POST /v1/systemone", "HTTPS, API key")
    Rel(lib, backstage, "catalog, techdocs, notifications", "HTTPS, static token")
    Rel(incidentio, oncall, "escalates")
    Rel(backstage, oncall, "notifies")
    UpdateLayoutConfig($c4ShapeInRow="3", $c4BoundaryInRow="1")
```

## Containers

| Container | Technology | Responsibilities | Configuration |
|---|---|---|---|
| Webhook receiver | axum on tokio | Signature verification, `webhook-id` deduplication (4,096 most recent), 202 acknowledgement, background execution of the flow, liveness endpoint | `SIGNALMAN_ADDR`, `INCIDENTIO_WEBHOOK_SECRET`, `--dry-run`, `--notify-owners` |
| CLI | clap | `triage` on a file with optional catalog enrichment, incident dedup candidates and forwarding; `incidentio whoami`, `open-incidents`, `triage-alert`; `backstage lookup`; `models` | Same variables; flags per command |
| Triage library | Rust crate `signalman` | Everything the two entry points share; the [component view](component.md) opens it | |

## Deployment notes

- One replica is enough for moderate alert volume; the binding limit is incident.io's 60 requests per minute on listing incidents, one call per triage.
- Horizontal scaling is safe for correctness: tag adds are idempotent, and duplicate deliveries across replicas cost a repeated model call, nothing else. The in-memory seen set is per replica.
- No persistent storage. The deployment's shape is a TOML file (a `ConfigMap`); secrets arrive as environment variables (a `Secret`); flags and variables override the file ([Configuration](../configuration.md)).
