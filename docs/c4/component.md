---
title: C4 level 3, components
description: Inside the signalman library, the modules that make up a triage, how they call one another, and which external endpoint each client owns.
status: current
last_reviewed: 2026-09-20
tags: [architecture, c4]
---

# C4 level 3: components

This level opens the triage library. The `Triager` orchestrates; everything else is a collaborator with one job.

```mermaid
C4Component
    title Components of the signalman library
    Container_Boundary(lib, "Triage library") {
        Component(triager, "Triager", "incidentio::sync", "Fetch fresh state, enrich, ask, decide, write back")
        Component(enricher, "Enricher", "backstage::enrich", "Resolve component, assemble owner candidates, select runbook, notify owner")
        Component(questions, "TriageQuestions", "triage::questions", "Build the fan-out with typed handles; read answers")
        Component(policy, "Policy / decide", "triage::policy", "Thresholds that turn answers into one Decision")
        Component(tsclient, "TypeSafe client", "client, question, answer", "Wire contract, Handle<A>, Probability, Confidence")
        Component(ioclient, "incident.io client", "incidentio::client", "Incidents, alerts, tags, attachments, alert events")
        Component(bsclient, "Backstage client", "backstage::client", "by-name, by-query, by-refs, TechDocs index, notifications")
        Component(webhook, "Webhook verifier", "incidentio::webhook", "Svix HMAC-SHA256, tolerance, envelope parsing")
        Component(http, "Retry loop", "http", "RetryPolicy, backoff, Retry-After")
    }
    System_Ext(incidentio, "incident.io")
    System_Ext(typesafe, "TypeSafe")
    System_Ext(backstage, "Backstage")
    Rel(triager, ioclient, "get alert, list incidents, add tags, attach")
    Rel(triager, enricher, "enrich(hints, text); notify_owner")
    Rel(triager, questions, "for_alert_with(alert, candidates); read(response)")
    Rel(triager, policy, "decide(answers)")
    Rel(triager, tsclient, "system_one(state, questions)")
    Rel(enricher, bsclient, "catalog, techdocs, notifications")
    Rel(questions, tsclient, "Questions, Handle<A>")
    Rel(tsclient, http, "send_with_retries")
    Rel(ioclient, http, "send_with_retries")
    Rel(bsclient, http, "send_with_retries")
    Rel(ioclient, incidentio, "REST v2")
    Rel(tsclient, typesafe, "REST v1")
    Rel(bsclient, backstage, "REST")
    UpdateLayoutConfig($c4ShapeInRow="3", $c4BoundaryInRow="1")
```

## Components

| Component | Depends on | Owns |
|---|---|---|
| `Triager` | incident.io client, Enricher, TriageQuestions, policy, TypeSafe client | The order of operations and the write-back rules; `Outcome` |
| `Enricher` | Backstage client, triage types | Hint resolution, neighbourhood walk, candidate rubric, runbook scoring, notification payloads |
| `TriageQuestions` | TypeSafe question builder | Which questions exist, when the speculative ones are asked, the instruction text |
| `Policy` and `decide` | Typed answers | Thresholds and their order of precedence |
| TypeSafe client | Retry loop | `Handle<A>`, `Response::get`, `Probability`, `Confidence`, the `options!` macro |
| incident.io client | Retry loop | Endpoint paths, per-endpoint authentication, error body parsing |
| Backstage client | Retry loop | Filter set encoding, cursor pagination, lenient entity types |
| Webhook verifier | none | Signature check pinned to Svix's published test vector; event envelope |
| Retry loop | reqwest | Which statuses are transient, how long to wait |

## Where the model boundary sits

Only `TriageQuestions` and the TypeSafe client know the model exists. `Policy` receives typed answers; `Triager` receives a `Decision`. Replacing the model, or adding a question, touches `questions.rs` and the policy field that reads it, nothing else.
