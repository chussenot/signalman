---
title: Configuration
description: Every environment variable rustsafe reads, which commands need it, and where to obtain the value.
status: current
last_reviewed: 2026-09-20
tags: [configuration]
---

# Configuration

Configuration is environment variables only. mise loads `.env` when present (`[env] _.file` in `mise.toml`). `.env` is gitignored and a pre-commit hook refuses to commit it; `.env.example` documents the variables.

| Variable | Required by | Default | Source |
|---|---|---|---|
| `TYPESAFE_API_KEY` | everything that calls the model | | console.typesafe.ai/keys |
| `TYPESAFE_BASE_URL` | | `https://api.typesafe.ai` | tests, proxies |
| `TYPESAFE_DEFAULT_MODEL` | | `jev-latest` | pin a version once thresholds are tuned |
| `INCIDENTIO_API_KEY` | `serve`, `incidentio *`, `triage --dedup-from-incidentio` | | Settings → API keys |
| `INCIDENTIO_BASE_URL` | | `https://api.incident.io` | tests, proxies |
| `INCIDENTIO_WEBHOOK_SECRET` | `serve` unless `--insecure-skip-verify` | | Settings → Webhooks → endpoint → signing secret, `whsec_...` |
| `INCIDENTIO_ALERT_SOURCE_CONFIG_ID` | `triage --forward-to-incidentio` | | the HTTP alert source's id |
| `INCIDENTIO_ALERT_SOURCE_TOKEN` | `triage --forward-to-incidentio` | | the HTTP alert source's secret token |
| `RUSTSAFE_ADDR` | `serve` | `127.0.0.1:8080` | |
| `RUST_LOG` | | `info` | tracing filter, for example `rustsafe=debug` |

incident.io API key scopes needed: view alerts, view incidents, manage alert tags, manage incident alerts. The key is never used for alert-source events; those use the source token.

Secrets are marked sensitive in HTTP headers and redacted from `Debug` output of both clients.
