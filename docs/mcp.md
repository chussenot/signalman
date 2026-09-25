---
title: MCP server
description: The Model Context Protocol tools signalman serves over stdio and Streamable HTTP, five always-on read-only tools and one gated write tool, what each one answers, the two transports and how they are served, how to configure and connect to them, and what still writes nothing on purpose.
status: current
last_reviewed: 2026-09-25
tags: [mcp, agents, decisions]
---

# MCP server

An agent investigating an alert needs signalman's judgments (who owns this, how bad is it, is it a duplicate) without parsing a log line or shelling out to the CLI. This page describes how those judgments are served over the [Model Context Protocol](https://modelcontextprotocol.io) (MCP): six tools, the same typed judgments the CLI and the webhook receiver produce. `signalman mcp` serves them alone, over stdio or Streamable HTTP; `signalman serve` also mounts them at `/mcp` next to the webhook receiver (see [Transports](#transports)). Five are always registered and read-only. A sixth, `apply_qualification`, writes to incident.io and is registered only when `mcp.allow_write` is turned on (default off; see [Writing](#writing)). Why the server exists and why it stops at these six is [decision 0008](decisions/0008-signalman-is-a-tool-for-agents.md): signalman is a tool for agents, not an agent. Every tool wraps a function the CLI subcommands already call, so there is one implementation of each capability.

Contract source: the official Rust SDK, [rmcp](https://github.com/modelcontextprotocol/rust-sdk) (`modelcontextprotocol/rust-sdk`), with its server, stdio transport and Streamable HTTP server features (`transport-streamable-http-server`). The binary carries no MCP client and no OAuth feature.

## Tools

Each tool answers one question an agent asks during an investigation, and each maps to a CLI subcommand or an outcome field so that what the agent sees is what an operator would see.

| Tool | Answers | Mirrors |
|---|---|---|
| `qualify_alert` | A full triage: the decision, every judgment with its probability, links, blast-radius context, and the tags that would be written. Give `alert_id` (an existing incident.io alert, triaged with full live context) or `alert` (a standalone alert body). Always a dry run. | `signalman triage <file> --json`, `signalman incidentio triage-alert` |
| `related_alerts` | Other alerts firing in incident.io within a window: the blast radius. Give `alert_id` (excludes that alert) or `component` (filters to it). | the `related_alerts` the flow puts in [the outcome](triage.md#the-outcome-contract) |
| `recent_changes` | Deploys, config changes and flag toggles posted to the [change feed](changes.md). Empty, with a reason, when the feed is not configured or nothing has been posted to this process. | the `recent_changes` field |
| `lookup_owner` | Catalog resolution for a component: its record, owner candidates, and the TechDocs runbook excerpt. Needs Backstage configured. | `signalman backstage lookup` |
| `open_incidents` | Incidents currently open in incident.io, the dedup candidate list. | `signalman incidentio open-incidents` |

Every tool in this table has `read_only_hint: true`. Every result carries the value as structured content (validated against the tool's declared output where one is fixed, such as `qualify_alert`'s [outcome contract](triage.md#the-outcome-contract)) and a short text summary, for a client that renders content instead of structured JSON. A tool that runs and fails (an unreachable upstream, a missing catalog entry) returns a tool-level error the client shows; a malformed request (both or neither of two mutually exclusive fields) is refused as a protocol-level error before anything runs. A TypeSafe answer that does not fit the questions (an owner or an incident the question never offered, a Score off its scale) is one of those tool-level errors: `qualify_alert` reports `TypeSafe call failed: …` with the client's message, which names the question and the option or level and ends with TypeSafe's request id. It fails there, in the client, rather than while reading the answer, and nothing is written ([Triage](triage.md#which-questions-are-asked)).

## Writing

An agent that has reviewed a `qualify_alert` result needs a way to apply it, and a tool that took free-form tags or note text would let a model write whatever it composed. `apply_qualification` is a sixth tool, registered only when `mcp.allow_write` is `true` (default `false`; see [Configuration](#configuration)). It applies a `qualify_alert` result: it writes the tags, rewrites the qualification note in place, and attaches the alert to an incident when the decision says so.

Input is `{ "alert_id": string, "outcome": <Outcome> }`, where `outcome` must be the exact [outcome document](triage.md#the-outcome-contract) a prior `qualify_alert(alert_id)` call returned for that same alert. The tool checks `outcome.alert.id == alert_id` and refuses (a protocol-level `invalid_params` error) if they differ, which also rules out `qualify_alert`'s standalone `alert` form: that form never has an alert id to match. It then revalidates the document (`Outcome::validate`) and rebuilds the decision from it; either failing is also `invalid_params`. Every write is re-derived from the document's own typed judgments — the tags, the attach target, the note content — so the tool cannot be used to write anything the outcome does not already say, and it never accepts a free-form instruction about what to write. It never creates an incident ([decision 0001](decisions/0001-incidentio-remains-the-alert-hub.md) still holds).

Output is a `Writes` object: `mode: "applied"`, `tags_applied` and `attached` booleans, and a `note` object (`status` of `created`, `replaced`, `failed`, `disabled` or `skipped`, an optional note `id`, an optional `error`; `replaced` is an earlier pass's note rewritten in place, `skipped` is a dry run or a path that never attempts a note). The `notified` and `forwarded` fields are always absent from this tool's result; they belong to a different write path and this tool does not send notifications.

Calling `apply_qualification` twice with the same outcome is safe: the tags and the attachment are idempotent on incident.io's side, and the note is replaced in place using the same marker-line template and "never stacked" behavior as the rest of the triage flow (`src/incidentio/note.rs`), never a second note. Its annotations reflect this: `read_only_hint: false`, `destructive_hint: false`, `idempotent_hint: true`, `open_world_hint: true`.

The typical shape for an agent is two calls: `qualify_alert(alert_id)` to get an outcome, then `apply_qualification(alert_id, outcome)` with that exact document to apply it.

`Server::new` always forces the wrapped `Triager`'s `write_back` to a dry run, so `qualify_alert`'s `alert_id` path (`Triager::triage_alert_by_id`) stays a read-only preview no matter how the rest of the process is configured. `mcp.allow_write` is a separate gate: `apply_qualification` does not go through `write_back` at all, it writes through the incident.io client directly, and it exists in the tool list only when `allow_write` was `true` at start-up. A deployment that wants every other tool to preview writes but still let an agent apply one explicitly reviewed outcome turns on `mcp.allow_write` alone; turning off `mcp.allow_write` is what keeps the server entirely read-only.

signalman records the calling MCP client's name and version (read once from `initialize`) in a `tracing::info!` line alongside the alert id and decision, for audit purposes. This is a log line, not part of the note.

## Configuration

```toml
[mcp]
enabled = true      # a kill switch; signalman mcp still has to be invoked, and serve mounts /mcp only when true
transport = "stdio" # how `signalman mcp` serves: "stdio" or "http"; `serve` ignores it
# bind_address = "0.0.0.0:8081"          # required by `signalman mcp` with transport = "http"; unused otherwise
# allowed_hosts = ["signalman.example.com"] # Host-header allow list for /mcp; empty (default) disables the check
allow_write = false # registers apply_qualification when true; off by default
```

`SIGNALMAN_MCP_ENABLED`, `SIGNALMAN_MCP_TRANSPORT`, `SIGNALMAN_MCP_BIND_ADDRESS`, `SIGNALMAN_MCP_ALLOWED_HOSTS` (comma-separated) and `SIGNALMAN_MCP_ALLOW_WRITE` override the file; see [Configuration](configuration.md#mcp). `allow_write` is the only setting that changes which tools exist: turning it on registers `apply_qualification` (see [Writing](#writing)); leaving it off keeps the server entirely read-only. It applies over both transports. Everything else the tools need, the TypeSafe model, the software catalog, the change feed, the routing thresholds, is the same `[typesafe]`, `[incidentio]`, `[backstage]`, `[flow]`, `[policy]` and `[triage]` configuration the rest of the binary reads. Secrets (`TYPESAFE_API_KEY`, `INCIDENTIO_API_KEY`, `BACKSTAGE_TOKEN`) are environment only, as everywhere else; so is `SIGNALMAN_MCP_TOKEN`, the bearer token the HTTP transport requires (see [Transports](#transports)).

`recent_changes` depends on where the server runs, because the [change feed](changes.md) is per-process, in-memory state. Mounted at `/mcp` by `signalman serve`, the tool reads the same change log `POST /changes` writes in that process, so it is live; this is the first configuration in which it is. A `signalman mcp` process, over stdio or over HTTP, never receives `POST /changes`, so there the tool always reports an empty list with a note explaining why.

## Transports

Two transports exist because agents run in two places: on an engineer's machine, where a client can launch a process and own it, and in a platform, where many clients reach one deployment over the network and a process-per-client is not an option. The same `Server` (`src/mcp.rs`) is reachable two ways. Over **stdio**, `signalman mcp` speaks newline-delimited JSON-RPC on stdin and stdout, one client per process, trusted by the process boundary; the client launches it. Over **Streamable HTTP** (`src/mcp/http.rs`), the server is mounted at `POST /mcp` on an axum router behind a bearer token, and any number of clients reach it over the network. The tool list is identical: `qualify_alert` is a forced dry run over HTTP exactly as over stdio, and `apply_qualification` is registered over HTTP if and only if `mcp.allow_write` is true.

HTTP is served in two configurations:

| Process | When `/mcp` is served | Listener | Change feed |
|---|---|---|---|
| `signalman serve` | `mcp.enabled` is true (default) and `SIGNALMAN_MCP_TOKEN` is set | `server.addr`, next to `POST /webhooks/incidentio` and `POST /changes`; `mcp.transport` and `mcp.bind_address` are ignored | shared: `recent_changes` reads what `POST /changes` recorded in this process |
| `signalman mcp` with `mcp.transport = "http"` | `mcp.enabled` is true (the command refuses to start otherwise); start-up fails naming `mcp.bind_address` (`SIGNALMAN_MCP_BIND_ADDRESS`) or `SIGNALMAN_MCP_TOKEN` when either is unset | `mcp.bind_address`, serving `/mcp` and `GET /healthz` only | none: no webhook route, no `POST /changes`; `recent_changes` is empty as over stdio, and the start-up log says so |

`serve` logs at start-up whether `/mcp` was mounted and, if not, why (`SIGNALMAN_MCP_TOKEN` unset, or `mcp.enabled = false`). The MCP-only HTTP process deliberately has no `/changes` route of its own: a process that needs a live feed runs `serve`, which hosts `/mcp` next to `POST /changes`. One place to post changes, one place to be reached.

**Stateless by construction.** A session per client would force replicas behind one Service to share session state or pin clients to a replica, and would give a graceful shutdown streams to drain. Nothing a signalman tool does needs a session: every call is one request with one typed answer. So the endpoint runs rmcp's `StreamableHttpService` with legacy session mode off, JSON responses on and a session manager that never creates a session. There is no `Mcp-Session-Id`; each `POST /mcp` is one JSON-RPC request answered by one JSON response. rmcp falls back to an SSE stream for a request only if the handler emits a notification before its result, which no signalman tool does. The consequences: two replicas behind one Kubernetes Service need no session affinity, and a graceful shutdown has no long-lived streams to drain. `GET /mcp` and `DELETE /mcp` answer `405` for the same reason: in the Streamable HTTP protocol `GET` opens a standalone server-to-client stream and `DELETE` ends a session, and a stateless server has neither to offer (`tests/mcp_http.rs` asserts both). The MCP specification allows a session-less server; Claude Code and rmcp clients handle it.

**The token.** `SIGNALMAN_MCP_TOKEN` is environment only, like every other secret ([decision 0006](decisions/0006-layered-configuration.md)). A token-checking layer sits in front of rmcp's service, so a request without `Authorization: Bearer <token>` (compared in constant time) never reaches the protocol handler; it is refused with `401` and `WWW-Authenticate: Bearer`. It is a separate secret from `INCIDENTIO_WEBHOOK_SECRET` (incident.io authenticating to signalman) and from `SIGNALMAN_CHANGES_TOKEN` (delivery tooling authenticating to signalman): an agent is a third party with its own credential, revocable on its own. A test proves the feed token does not open `/mcp` and the MCP token does not open `/changes`.

**`allowed_hosts`.** `mcp.allowed_hosts` (`SIGNALMAN_MCP_ALLOWED_HOSTS`, comma-separated) is rmcp's DNS-rebinding guard on the `Host` header, hostnames or `host:port`. Empty, the default, disables the check. That is deliberate: the guard exists for local servers a browser could be tricked into reaching, and a browser cannot attach this bearer token, so the token already closes that door. Operators who want to pin the hostnames the endpoint answers to set it anyway; a test shows a request to `127.0.0.1` refused when only `signalman.example.com` is listed.

The HTTP transport is covered by `tests/mcp_http.rs` on a real TCP listener. Like everything else in this repository, it has not been exercised against a live account.

## Connecting a client

### Claude Code

Over stdio, add to `.mcp.json` in the repository, or the equivalent user-level configuration:

```json
{
  "mcpServers": {
    "signalman": {
      "command": "signalman",
      "args": ["mcp"],
      "env": {
        "TYPESAFE_API_KEY": "…",
        "INCIDENTIO_API_KEY": "…",
        "BACKSTAGE_TOKEN": "…"
      }
    }
  }
}
```

`BACKSTAGE_TOKEN` is only needed when the catalog backend requires authentication. The server should read the same configuration file `serve` uses, so that the agent and the webhook receiver share one reviewed set of thresholds and wording. Either way of naming it works for a launched process: `--config` is a global flag, so `"args": ["mcp", "--config", "/etc/signalman/config.toml"]`, or `SIGNALMAN_CONFIG` in the `env` block; without both, `./signalman.toml` and then `/etc/signalman/config.toml` are tried ([Configuration](configuration.md#the-file)).

Over HTTP, point at a running `serve` or `signalman mcp` and send the token:

```json
{
  "mcpServers": {
    "signalman": {
      "type": "http",
      "url": "https://signalman.example.com/mcp",
      "headers": { "Authorization": "Bearer ${SIGNALMAN_MCP_TOKEN}" }
    }
  }
}
```

Claude Code expands `${VAR}` from its environment when it reads `.mcp.json`, so the token stays out of the file. No secrets for TypeSafe, incident.io or Backstage are needed on the client side: the server process holds them.

### rmcp clients

`StreamableHttpClientTransportConfig::auth_header` takes the bare token; rmcp prepends `Bearer ` itself.

### incident.io's own AI features

incident.io has been adding AI and assistant features to the product; whether and how it lets an organisation register an external MCP server is a question for incident.io's current documentation, not something signalman controls or has verified. If it does, signalman is a standard MCP server over stdio or Streamable HTTP and needs nothing special to be pointed at.

## Trying it locally

Over stdio:

```sh
mise run build
echo '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"smoke-test","version":"0"}}}' \
  | TYPESAFE_API_KEY=… INCIDENTIO_API_KEY=… target/release/signalman mcp
```

prints the `initialize` response and exits once stdin closes.

Over HTTP, with `serve` running on the default address and `SIGNALMAN_MCP_TOKEN` exported in both shells:

```sh
curl -sS http://127.0.0.1:8080/mcp \
  -H "authorization: Bearer $SIGNALMAN_MCP_TOKEN" \
  -H "content-type: application/json" -H "accept: application/json, text/event-stream" \
  -d '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"smoke-test","version":"0"}}}'
```

prints the same `initialize` response as one JSON body. Without the header the answer is `401` with `WWW-Authenticate: Bearer`.

For interactive exploration, the [MCP Inspector](https://modelcontextprotocol.io/legacy/tools/inspector) speaks the same protocol. Over stdio: `npx @modelcontextprotocol/inspector signalman mcp`. Over HTTP: `npx @modelcontextprotocol/inspector --transport http --server-url http://127.0.0.1:8081/mcp`, then add the `Authorization` header in its UI.

## What is not here

- **Writing, beyond `apply_qualification`.** No tool notifies an owner; no other tool tags, writes a note or attaches an incident.
- **OAuth or OIDC in front of `/mcp`.** The endpoint checks one bearer token and nothing else. Put an ingress or gateway in front of it for per-agent identities, TLS termination or rate limits.
- **Investigation or remediation.** A tool answers one typed question and does not chain calls or choose the next step; that is the calling agent's job ([decision 0008](decisions/0008-signalman-is-a-tool-for-agents.md)).
