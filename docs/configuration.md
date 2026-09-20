---
title: Configuration
description: Every environment variable signalman reads, which commands need it, its default, and where to obtain the value.
status: current
last_reviewed: 2026-09-20
tags: [configuration]
---

# Configuration

Configuration is environment variables only. mise loads `.env` when present (`[env] _.file` in `mise.toml`). `.env` is gitignored, a pre-commit hook refuses to commit it, and `.env.example` documents every variable.

## TypeSafe

| Variable | Required by | Default | Source |
|---|---|---|---|
| `TYPESAFE_API_KEY` | everything that calls the model | | console.typesafe.ai/keys |
| `TYPESAFE_BASE_URL` | | `https://api.typesafe.ai` | tests, proxies |
| `TYPESAFE_DEFAULT_MODEL` | | `jev-latest` | pin a version once thresholds are tuned |

## incident.io

| Variable | Required by | Default | Source |
|---|---|---|---|
| `INCIDENTIO_API_KEY` | `serve`, `incidentio *`, `triage --dedup-from-incidentio` | | Settings → API keys; scopes: view alerts, view incidents, manage alert tags, manage incident alerts, manage alert notes |
| `INCIDENTIO_BASE_URL` | | `https://api.incident.io` | tests, proxies |
| `INCIDENTIO_WEBHOOK_SECRET` | `serve` unless `--insecure-skip-verify` | | Settings → Webhooks → endpoint → signing secret, `whsec_...` |
| `INCIDENTIO_ALERT_SOURCE_CONFIG_ID` | `triage --forward-to-incidentio` | | the HTTP alert source's id |
| `INCIDENTIO_ALERT_SOURCE_TOKEN` | `triage --forward-to-incidentio` | | the HTTP alert source's secret token |

The API key is never used for alert-source events; those use the source token.

## Backstage

| Variable | Required by | Default | Source |
|---|---|---|---|
| `BACKSTAGE_BASE_URL` | `serve` (enables enrichment when set), `triage --enrich-from-backstage`, `backstage lookup` | | backend URL without `/api` |
| `BACKSTAGE_TOKEN` | same | | `backend.auth.externalAccess` static token; optional for unauthenticated development backends |
| `BACKSTAGE_NAMESPACE` | same | `default` | namespace tried first for bare component names |
| `BACKSTAGE_NOTIFY` | `serve` | `false` | notify the owning group after page, ticket and human-triage decisions; also `--notify-owners` |
| `BACKSTAGE_APP_URL` | links in the qualification note | `BACKSTAGE_BASE_URL` | the frontend URL when it differs from the backend, for example `https://backstage.example.com` against `https://backstage-backend.example.com` |
| `SIGNALMAN_COMPONENT_KEYS` | enrichment | `component,service,app,application` | alert attribute or label names that carry the component identity |

## Process

| Variable | Required by | Default | Source |
|---|---|---|---|
| `SIGNALMAN_ADDR` | `serve` | `127.0.0.1:8080` | |
| `SIGNALMAN_NO_NOTE` | `serve`, `incidentio triage-alert` | `false` | skip the qualification note; also `--no-note` |
| `SIGNALMAN_RELATED_WINDOW_MINUTES` | `serve`, `incidentio triage-alert` | `30` | window for related firing alerts; `0` disables the lookup; also `--related-window-minutes` |
| `RUST_LOG` | | `info` | tracing filter, for example `signalman=debug` |

Secrets are marked sensitive in HTTP headers and redacted from the `Debug` output of every client.
