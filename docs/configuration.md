---
title: Configuration
description: The four configuration layers and their precedence, every setting with its file key, environment variable, flag and default, what is file-only and why, how secrets are handled, and how to validate a configuration before rollout.
status: current
last_reviewed: 2026-09-23
tags: [configuration, kubernetes]
---

# Configuration

signalman resolves every setting once, at start-up, from four layers. A later layer wins:

| Layer | Purpose | Example |
|---|---|---|
| 1. Built-in default | runs with nothing configured | `127.0.0.1:8080` |
| 2. Configuration file (TOML) | the reviewed shape of a deployment: URLs, thresholds, wording, team list | a `ConfigMap` mounted at `/etc/signalman/config.toml` |
| 3. Environment variable | how one deployment or one operator deviates from the file without editing it | `SIGNALMAN_RELATED_WINDOW_MINUTES=10` |
| 4. Command-line flag | a one-off for the person at the keyboard | `--dry-run`, `--related-window-minutes 0` |

The file sits below the environment on purpose: the file is shared and reviewed, the environment is local to a pod or a shell. An empty environment variable counts as unset, because Kubernetes and shells often export a name with no value. A set variable that does not parse is an error naming the variable, its value and the file key it maps to; it never falls back to a default. `src/config.rs` is the only module that reads a non-secret environment variable, so this page describes one function ([decision 0006](decisions/0006-layered-configuration.md)).

## Secrets

Secrets are environment only. The file schema has no key for them; `api_key = …` in the file is rejected as an unknown key. `signalman config show` never prints them.

| Variable | Required by | Source |
|---|---|---|
| `TYPESAFE_API_KEY` | everything that calls the model | console.typesafe.ai/keys |
| `INCIDENTIO_API_KEY` | `serve`, `incidentio *`, `triage --dedup-from-incidentio` | Settings → API keys; scopes: view alerts, view incidents, manage alert tags, manage incident alerts, manage alert notes |
| `INCIDENTIO_WEBHOOK_SECRET` | `serve` unless `--insecure-skip-verify` | Settings → Webhooks → endpoint → signing secret, `whsec_...` |
| `INCIDENTIO_ALERT_SOURCE_CONFIG_ID`, `INCIDENTIO_ALERT_SOURCE_TOKEN` | `triage --forward-to-incidentio` | the HTTP alert source's id and token |
| `BACKSTAGE_TOKEN` | Backstage enrichment, unless the backend is unauthenticated | `backend.auth.externalAccess` static token |
| `SIGNALMAN_CHANGES_TOKEN` | the [change feed](changes.md); unset leaves `POST /changes` unrouted | any long random string, given to the delivery tools that post changes |
| `SIGNALMAN_MCP_TOKEN` | the [MCP server](mcp.md#transports) over HTTP: `serve` mounts `/mcp` only when it is set; `mcp` with `transport = "http"` fails at start-up without it | any long random string, given to the agents' MCP client configuration; a different secret from the two above |

Secrets are marked sensitive in HTTP headers and redacted from the `Debug` output of every client. In Kubernetes they come from a `Secret` through `envFrom`; see [Operations](operations.md#kubernetes).

## The file

TOML, every key optional, unknown keys rejected. Location, first match wins: `--config PATH`, then `SIGNALMAN_CONFIG`, then `./signalman.toml`, then `/etc/signalman/config.toml`. A file named explicitly must exist; the implicit paths may be absent. [`examples/config/signalman.toml`](https://github.com/chussenot/rustsafe/blob/main/examples/config/signalman.toml) shows every key with its default and the variable that overrides it.

## Settings

### `[server]`

| File key | Environment | Flag | Default | Meaning |
|---|---|---|---|---|
| `server.addr` | `SIGNALMAN_ADDR` | `serve --addr` | `127.0.0.1:8080` | listen address |
| `server.max_concurrent_triages` | `SIGNALMAN_MAX_CONCURRENT_TRIAGES` | | `8` | triages running at once; at least 1 |
| `server.max_queued_triages` | `SIGNALMAN_MAX_QUEUED_TRIAGES` | | `64` | triages waiting for a slot; a delivery beyond that is refused with `503` and `Retry-After` so incident.io retries it |
| `server.triage_timeout_seconds` | `SIGNALMAN_TRIAGE_TIMEOUT_SECONDS` | | `60` | deadline for one triage, every upstream call and retry included; at least 1 |

### `[typesafe]`

| File key | Environment | Flag | Default | Meaning |
|---|---|---|---|---|
| `typesafe.base_url` | `TYPESAFE_BASE_URL` | | `https://api.typesafe.ai` | tests, proxies |
| `typesafe.model` | `TYPESAFE_DEFAULT_MODEL` | `triage --model` | `jev-latest` | pin a version once thresholds are tuned |
| `typesafe.timeout_seconds` | `TYPESAFE_TIMEOUT_SECONDS` | | `10` | per-attempt timeout |

### `[incidentio]`

| File key | Environment | Flag | Default | Meaning |
|---|---|---|---|---|
| `incidentio.base_url` | `INCIDENTIO_BASE_URL` | | `https://api.incident.io` | tests, proxies |
| `incidentio.max_candidates` | `SIGNALMAN_MAX_CANDIDATES` | `incidentio open-incidents --max` | `40` | open incidents offered as dedup candidates |

### `[backstage]`

Setting `backstage.base_url` (or its variable) turns catalog enrichment on.

| File key | Environment | Flag | Default | Meaning |
|---|---|---|---|---|
| `backstage.base_url` | `BACKSTAGE_BASE_URL` | | unset | backend URL without `/api` |
| `backstage.app_url` | `BACKSTAGE_APP_URL` | | `base_url` | frontend URL for links in the qualification note |
| `backstage.namespace` | `BACKSTAGE_NAMESPACE` | | `default` | namespace tried first for bare component names |
| `backstage.component_keys` | `SIGNALMAN_COMPONENT_KEYS` (comma-separated) | | `component,service,app,application,Service,Component` | alert attribute or label names carrying the component identity |
| `backstage.notify` | `BACKSTAGE_NOTIFY` | `serve --notify-owners` | `false` | notify the owning group after page, ticket and human-triage decisions |

### `[flow]`

| File key | Environment | Flag | Default | Meaning |
|---|---|---|---|---|
| `flow.note` | `SIGNALMAN_NOTE` | `--no-note` | `true` | write the qualification note |
| `flow.related_window_minutes` | `SIGNALMAN_RELATED_WINDOW_MINUTES` | `--related-window-minutes` | `30` | window for related firing alerts; `0` disables the lookup |
| `flow.related_max` | `SIGNALMAN_RELATED_MAX` | | `20` | cap on related alerts put in the state |
| `flow.change_window_minutes` | `SIGNALMAN_CHANGE_WINDOW_MINUTES` | | `120` | how far back a posted change may lie to be offered as a cause; `0` disables |
| `flow.change_max` | `SIGNALMAN_CHANGE_MAX` | | `10` | cap on changes put in the state |

### `[mcp]`

signalman serves its typed capabilities over the Model Context Protocol ([MCP](mcp.md)), decision 0008: five read-only tools and one write tool gated by `mcp.allow_write`. `signalman mcp` serves them alone, over stdio or Streamable HTTP; `signalman serve` also mounts them at `/mcp` on its own listener whenever `mcp.enabled` is true and `SIGNALMAN_MCP_TOKEN` is set ([Transports](mcp.md#transports)).

| File key | Environment | Default | Meaning |
|---|---|---|---|
| `mcp.enabled` | `SIGNALMAN_MCP_ENABLED` | `true` | a kill switch: `signalman mcp` still has to be invoked, nothing auto-starts it, but a deployment can force it off; `false` also stops `serve` from mounting `/mcp` |
| `mcp.transport` | `SIGNALMAN_MCP_TRANSPORT` | `"stdio"` | how `signalman mcp` serves: `"stdio"` (one client per process) or `"http"` (Streamable HTTP at `/mcp` on `mcp.bind_address`, behind `SIGNALMAN_MCP_TOKEN`); `serve` ignores it |
| `mcp.bind_address` | `SIGNALMAN_MCP_BIND_ADDRESS` | unset | listen address for `signalman mcp` with `"http"`; required there, start-up fails naming it when unset; unused by `"stdio"` and by `serve`, which uses `server.addr` |
| `mcp.allowed_hosts` | `SIGNALMAN_MCP_ALLOWED_HOSTS` (comma-separated) | empty | hostnames or `host:port` the `/mcp` endpoint answers to, checked on the `Host` header (rmcp's DNS-rebinding guard); empty disables the check, because the bearer token already defeats that attack |
| `mcp.allow_write` | `SIGNALMAN_MCP_ALLOW_WRITE` | `false` | registers the `apply_qualification` write tool; an MCP client that can call it can write alert tags, a qualification note, and an incident attachment (never create an incident, decision 0001); off by default |

### `[policy]`, file only

Routing thresholds; absent keys keep their defaults. Every probability is validated to `0..=1`, and `human_below_confidence` may not exceed `auto_route_confidence`. What each one does is in [Triage](triage.md#the-decision).

| File key | Default |
|---|---|
| `policy.suppress_below` | `0.25` |
| `policy.attach_confidence` | `0.75` |
| `policy.auto_route_confidence` | `0.70` |
| `policy.human_below_confidence` | `0.40` |
| `policy.page_at` | `"major"` (`none`, `minor`, `major`, `outage`) |
| `policy.flag_change_above` | `0.65` |

### `[triage]`, file only

`[triage.text]` overrides the wording of any question: `owner_question`, `owner_guidance`, `owner_catalog_guidance`, `impact_question`, `impact_related_context`, `impact_levels` (exactly four, lowest first), `actionable_question`, `actionable_yes`, `actionable_no`, `duplicate_question`, `duplicate_none`, `change_question`. The set of questions and their types are not configurable; [Triage](triage.md#what-is-configurable) explains why.

`[[triage.teams]]` entries (`key`, `label`, `description`) replace the built-in fallback owner list used when no catalog is configured or nothing in it matched. `none_of_these` is appended automatically and may not be defined.

Thresholds and wording have no environment variables: they are reviewed as a unit in the file, not toggled per pod.

## Process

| Variable | Default | Meaning |
|---|---|---|
| `SIGNALMAN_CONFIG` | | path of the configuration file when `--config` is not given |
| `RUST_LOG` | `info` | tracing filter, for example `signalman=debug` |

## Validating before rollout

```sh
signalman config show --config deploy/config.toml
```

prints the effective configuration as TOML after every layer, headed by the file used and the environment variables that contributed, and exits non-zero on an unknown key, a wrong type, an out-of-range threshold or an unparsable variable. Run it in the pipeline that renders the `ConfigMap`. Locally, mise loads `.env` when present (`[env] _.file` in `mise.toml`); `.env` is gitignored, a pre-commit hook refuses to commit it, and `.env.example` lists the secrets and the most common variables.
