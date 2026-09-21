---
title: Operations
description: Running the webhook receiver, its endpoints and manual commands, the upstream limits that bound throughput, what the logs contain, and how each failure shows up.
status: current
last_reviewed: 2026-09-20
tags: [operations]
---

# Operations

## Running

```sh
signalman serve --addr 0.0.0.0:8080                 # production shape
signalman serve --dry-run                            # decide, write nothing
signalman serve --notify-owners                      # also notify groups through Backstage
signalman serve --insecure-skip-verify               # local only; logs a warning on start
signalman serve --config /etc/signalman/config.toml  # explicit file; see Configuration for the search order
```

Startup fails fast on a missing secret, an unknown key in the configuration file or an unparsable environment value. The process exits cleanly on SIGTERM or Ctrl-C. Backstage enrichment is on whenever `backstage.base_url` is set, in the file or through `BACKSTAGE_BASE_URL`.

## Endpoints

| Path | Purpose |
|---|---|
| `POST /webhooks/incidentio` | delivery endpoint |
| `GET /healthz` | liveness; returns `ok` |
| `POST /changes`, `GET /changes` | the [change feed](changes.md); routed only when `SIGNALMAN_CHANGES_TOKEN` is set; bearer token |
| `POST /changes/argocd`, `POST /changes/gitlab` | native adapters: Argo CD `Application` (bearer) and GitLab webhooks (`X-Gitlab-Token`) |

There is no readiness endpoint yet; upstream reachability is only known when a flow runs.

## Manual commands

```sh
signalman incidentio whoami                              # API key and roles
signalman incidentio open-incidents                      # dedup candidates as the flow sees them
signalman incidentio triage-alert <alert id> --dry-run   # the webhook flow for one alert, no write-back
signalman backstage lookup <component> --text "<alert>"  # what the catalog contributes
signalman triage examples/alerts/dns.json --print-request  # exact TypeSafe request, no call
signalman config show                                    # effective configuration after every layer
signalman eval examples/eval/cases.jsonl --record runs/x  # grade labelled alerts, keep raw responses
```

`incidentio triage-alert` is also the way to retry a triage that failed after the webhook was acknowledged.

## Kubernetes

The shape of a deployment is a `ConfigMap`; the secrets are a `Secret`; a change to either rolls the pods. Nothing below has been run against a cluster yet; it follows the configuration contract in [Configuration](configuration.md).

```yaml
apiVersion: v1
kind: ConfigMap
metadata:
  name: signalman-config
data:
  config.toml: |
    [server]
    addr = "0.0.0.0:8080"
    [backstage]
    base_url = "http://backstage-backend.backstage.svc:7007"
    app_url = "https://backstage.example.com"
    notify = true
    [policy]
    page_at = "major"
---
apiVersion: v1
kind: Secret
metadata:
  name: signalman-secrets
type: Opaque
stringData:
  TYPESAFE_API_KEY: "…"
  INCIDENTIO_API_KEY: "…"
  INCIDENTIO_WEBHOOK_SECRET: "whsec_…"
  BACKSTAGE_TOKEN: "…"
---
apiVersion: apps/v1
kind: Deployment
metadata:
  name: signalman
spec:
  replicas: 2
  selector:
    matchLabels: { app: signalman }
  template:
    metadata:
      labels: { app: signalman }
      annotations:
        # Rendered by the pipeline (Helm: sha256sum of the ConfigMap); a
        # changed value rolls the pods, which is how a new config takes effect.
        checksum/config: "<sha256 of config.toml>"
    spec:
      containers:
        - name: signalman
          image: ghcr.io/example/signalman:0.4.0
          args: ["serve"]                       # finds /etc/signalman/config.toml
          envFrom:
            - secretRef: { name: signalman-secrets }
          env:
            - name: SIGNALMAN_RELATED_WINDOW_MINUTES   # a per-environment override, above the file
              value: "15"
          ports:
            - containerPort: 8080
          volumeMounts:
            - name: config
              mountPath: /etc/signalman
              readOnly: true
          livenessProbe:
            httpGet: { path: /healthz, port: 8080 }
          readinessProbe:
            httpGet: { path: /healthz, port: 8080 }   # no upstream check yet (roadmap)
      volumes:
        - name: config
          configMap: { name: signalman-config }
```

Validate the rendered file in the pipeline before applying: `signalman config show --config config.toml` exits non-zero on an unknown key, a wrong type or an out-of-range threshold. There is no hot reload by design: a rollout is the unit of change, visible in the deployment history.

## Limits that bound throughput

| Limit | Value | Consequence |
|---|---|---|
| incident.io API key | 1,200 requests/min | shared by all calls |
| incident.io list incidents | 60 requests/min | one call per triage; the binding limit at volume |
| incident.io list alerts | shared key limit | one call per triage for related firing alerts; `--related-window-minutes 0` removes it |
| incident.io alert notes | shared key limit | two calls per triage: list, then create or replace; `--no-note` removes both |
| incident.io alert source events | 60 burst, 120/min per source | `--forward-to-incidentio` only |
| TypeSafe | 1,200 requests/min; 64k tokens per request | one request per triage, roughly 1 to 4k tokens with catalog context |
| Backstage | instance dependent | up to five catalog calls and one TechDocs call per triage |
| Change feed | 1,000 newest changes per replica, in memory | older changes evicted; a restart forgets the window |
| Background triage concurrency | unbounded today | see roadmap |

## Logs

`tracing` to stderr, filtered by `RUST_LOG` (default `info`). One line per triage carries the alert id, title, decision, resolved component, attachment, notification recipient, whether write-back applied, and the versioned model. Rejected webhooks log the reason and `webhook-id`. Retries log attempt, status and delay at `warn`. Catalog enrichment details are at `debug`.

## Failure modes

| Situation | Behaviour |
|---|---|
| Bad signature or missing headers | 401; incident.io retries for 24 h; fix the secret |
| Unparseable body | 400; incident.io retries; check the event subscription |
| Duplicate delivery | 200, no work |
| Component not in the catalog | triage continues with all team groups as candidates |
| Backstage transport or auth error | triage fails and is logged; the alert stays untagged |
| TypeSafe or incident.io error in the flow | logged at `error`; alert stays untagged; no retry because the delivery was already acknowledged |
| Notification fails | logged at `warn`; tags and attachment stay |
| Process restart | the in-memory `webhook-id` set is lost; a resend within the window is processed again; tag adds are idempotent |
