---
title: MCP server
description: The Model Context Protocol tools signalman serves over stdio — five always-on read-only tools and one gated write tool — what each one answers, how to configure and connect to them, and what still writes nothing on purpose.
status: current
last_reviewed: 2026-09-22
tags: [mcp, agents, decisions]
---

# MCP server

`signalman mcp` serves six tools over the [Model Context Protocol](https://modelcontextprotocol.io) (MCP): the same typed judgments the CLI and the webhook receiver produce, callable from an agent. Five are always registered and read-only. A sixth, `apply_qualification`, writes to incident.io and is registered only when `mcp.allow_write` is turned on (default off; see [Writing](#writing)). [Decision 0008](decisions/0008-signalman-is-a-tool-for-agents.md) is why this exists and why it stops here: signalman is a tool for agents, not an agent. No tool creates an incident, and none runs a generative model. Every tool is a thin wrapper over a function the CLI subcommands already call, so there is exactly one implementation of each capability.

Contract source: the official Rust SDK, [rmcp](https://github.com/modelcontextprotocol/rust-sdk) (`modelcontextprotocol/rust-sdk`), server and stdio transport features only.

## Tools

| Tool | Answers | Mirrors |
|---|---|---|
| `qualify_alert` | A full triage: the decision, every judgment with its probability, links, blast-radius context, and the tags that would be written. Give `alert_id` (an existing incident.io alert, triaged with full live context) or `alert` (a standalone alert body). Always a dry run. | `signalman triage <file> --json`, `signalman incidentio triage-alert` |
| `related_alerts` | Other alerts firing in incident.io within a window: the blast radius. Give `alert_id` (excludes that alert) or `component` (filters to it). | the `related_alerts` the flow puts in [the outcome](triage.md#the-outcome-contract) |
| `recent_changes` | Deploys, config changes and flag toggles posted to the [change feed](changes.md). Empty, with a reason, when the feed is not configured or nothing has been posted to this process. | the `recent_changes` field |
| `lookup_owner` | Catalog resolution for a component: its record, owner candidates, and the TechDocs runbook excerpt. Needs Backstage configured. | `signalman backstage lookup` |
| `open_incidents` | Incidents currently open in incident.io, the dedup candidate list. | `signalman incidentio open-incidents` |

Every tool in this table has `read_only_hint: true`. Every result carries the value as structured content (validated against the tool's declared output where one is fixed, such as `qualify_alert`'s [outcome contract](triage.md#the-outcome-contract)) and a short text summary, for a client that renders content instead of structured JSON. A tool that runs and fails (an unreachable upstream, a missing catalog entry) returns a tool-level error the client shows; a malformed request (both or neither of two mutually exclusive fields) is refused as a protocol-level error before anything runs.

## Writing

`apply_qualification` is a sixth tool, registered only when `mcp.allow_write` is `true` (default `false`; see [Configuration](#configuration)). It applies a `qualify_alert` result: it writes the tags, rewrites the qualification note in place, and attaches the alert to an incident when the decision says so.

Input is `{ "alert_id": string, "outcome": <Outcome> }`, where `outcome` must be the exact [outcome document](triage.md#the-outcome-contract) a prior `qualify_alert(alert_id)` call returned for that same alert. The tool checks `outcome.alert.id == alert_id` and refuses (a protocol-level `invalid_params` error) if they differ, which also rules out `qualify_alert`'s standalone `alert` form: that form never has an alert id to match. It then revalidates the document (`Outcome::validate`) and rebuilds the decision from it; either failing is also `invalid_params`. Every write is re-derived from the document's own typed judgments — the tags, the attach target, the note content — so the tool cannot be used to write anything the outcome does not already say, and it never accepts a free-form instruction about what to write. It never creates an incident ([decision 0001](decisions/0001-incidentio-remains-the-alert-hub.md) still holds).

Output is a `Writes` object: `mode: "applied"`, `tags_applied` and `attached` booleans, and a `note` object (`status` of `created`, `updated`, `disabled` or `failed`, an optional note `id`, an optional `error`). The `notified` and `forwarded` fields are always absent from this tool's result; they belong to a different write path and this tool does not send notifications.

Calling `apply_qualification` twice with the same outcome is safe: the tags and the attachment are idempotent on incident.io's side, and the note is replaced in place using the same marker-line template and "never stacked" behavior as the rest of the triage flow (`src/incidentio/note.rs`), never a second note. Its annotations reflect this: `read_only_hint: false`, `destructive_hint: false`, `idempotent_hint: true`, `open_world_hint: true`.

The typical shape for an agent is two calls: `qualify_alert(alert_id)` to get an outcome, then `apply_qualification(alert_id, outcome)` with that exact document to apply it.

`Server::new` always forces the wrapped `Triager`'s `write_back` to a dry run, so `qualify_alert`'s `alert_id` path (`Triager::triage_alert_by_id`) stays a read-only preview no matter how the rest of the process is configured. `mcp.allow_write` is a separate gate: `apply_qualification` does not go through `write_back` at all, it writes through the incident.io client directly, and it exists in the tool list only when `allow_write` was `true` at start-up. A deployment that wants every other tool to preview writes but still let an agent apply one explicitly reviewed outcome turns on `mcp.allow_write` alone; turning off `mcp.allow_write` is what keeps the server entirely read-only.

signalman records the calling MCP client's name and version (read once from `initialize`) in a `tracing::info!` line alongside the alert id and decision, for audit purposes. This is a log line, not part of the note.

## Configuration

```toml
[mcp]
enabled = true      # a kill switch; signalman mcp still has to be invoked
transport = "stdio" # "http" is accepted but not yet implemented
allow_write = false # registers apply_qualification when true; off by default
```

`SIGNALMAN_MCP_ENABLED`, `SIGNALMAN_MCP_TRANSPORT`, `SIGNALMAN_MCP_BIND_ADDRESS`, `SIGNALMAN_MCP_ALLOW_WRITE` override the file; see [Configuration](configuration.md#mcp). `allow_write` is the only setting that changes which tools exist: turning it on registers `apply_qualification` (see [Writing](#writing)); leaving it off keeps the server entirely read-only. Everything else the tools need — the TypeSafe model, the software catalog, the change feed, the routing thresholds — is the same `[typesafe]`, `[incidentio]`, `[backstage]`, `[flow]`, `[policy]` and `[triage]` configuration the rest of the binary reads. Secrets (`TYPESAFE_API_KEY`, `INCIDENTIO_API_KEY`, `BACKSTAGE_TOKEN`) are environment only, as everywhere else.

`recent_changes` is honest about a real limitation: the [change feed](changes.md) is per-process, in-memory state. A `signalman mcp` process running over stdio, separate from `signalman serve`, never receives `POST /changes`, so the tool always reports an empty list with a note explaining why. Streamable HTTP, mounted on the same router `serve` runs, would share the feed; it is not implemented yet (`signalman-4gp.7`).

## Connecting a client

### Claude Code

Add to `.mcp.json` in the repository, or the equivalent user-level configuration:

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

`BACKSTAGE_TOKEN` is only needed when the catalog backend requires authentication. `SIGNALMAN_CONFIG` (or `--config` is not available for an MCP-launched process; use the environment variable) points at the same configuration file `serve` uses, so the two share one reviewed set of thresholds and wording.

### incident.io's own AI features

incident.io has been adding AI and assistant features to the product; whether and how it lets an organisation register an external MCP server is a question for incident.io's current documentation, not something signalman controls or has verified. If it does, `signalman mcp` is a standard stdio MCP server and needs nothing special to be pointed at.

## Trying it locally

```sh
mise run build
echo '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"smoke-test","version":"0"}}}' \
  | TYPESAFE_API_KEY=… INCIDENTIO_API_KEY=… target/release/signalman mcp
```

prints the `initialize` response and exits once stdin closes. For interactive exploration, the [MCP Inspector](https://modelcontextprotocol.io/legacy/tools/inspector) speaks the same protocol: `npx @modelcontextprotocol/inspector signalman mcp`.

## What is not here

- **Writing, beyond `apply_qualification`.** No tool notifies an owner, and no tool other than `apply_qualification` tags an alert, writes a note or attaches an incident. `apply_qualification` itself is gated behind `mcp.allow_write` (default off) and documented above under [Writing](#writing).
- **Streamable HTTP** (`signalman-4gp.7`). `mcp.transport = "http"` is accepted in configuration and fails clearly at start-up naming the bead; it would mount the same tools on the router `serve` already runs, under its own bearer-token secret, not the webhook signing secret (a webhook is incident.io authenticating to signalman, this is the reverse).
- **Investigation or remediation.** A tool answers one typed question; it does not decide what to do next, retry, or chain calls. That is the calling agent's job, which is the point of [decision 0008](decisions/0008-signalman-is-a-tool-for-agents.md).
