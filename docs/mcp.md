---
title: MCP server
description: The read-only Model Context Protocol tools signalman serves over stdio, what each one answers, how to configure and connect to them, and what still writes nothing on purpose.
status: current
last_reviewed: 2026-09-22
tags: [mcp, agents, decisions]
---

# MCP server

`signalman mcp` serves five read-only tools over the [Model Context Protocol](https://modelcontextprotocol.io) (MCP): the same typed judgments the CLI and the webhook receiver produce, callable from an agent. [Decision 0008](decisions/0008-signalman-is-a-tool-for-agents.md) is why this exists and why it stops here: signalman is a tool for agents, not an agent. No tool writes to incident.io or Backstage, none creates an incident, and none runs a generative model. Every tool is a thin wrapper over a function the CLI subcommands already call, so there is exactly one implementation of each capability.

Contract source: the official Rust SDK, [rmcp](https://github.com/modelcontextprotocol/rust-sdk) (`modelcontextprotocol/rust-sdk`), server and stdio transport features only.

## Tools

| Tool | Answers | Mirrors |
|---|---|---|
| `qualify_alert` | A full triage: the decision, every judgment with its probability, links, blast-radius context, and the tags that would be written. Give `alert_id` (an existing incident.io alert, triaged with full live context) or `alert` (a standalone alert body). Always a dry run. | `signalman triage <file> --json`, `signalman incidentio triage-alert` |
| `related_alerts` | Other alerts firing in incident.io within a window: the blast radius. Give `alert_id` (excludes that alert) or `component` (filters to it). | the `related_alerts` the flow puts in [the outcome](triage.md#the-outcome-contract) |
| `recent_changes` | Deploys, config changes and flag toggles posted to the [change feed](changes.md). Empty, with a reason, when the feed is not configured or nothing has been posted to this process. | the `recent_changes` field |
| `lookup_owner` | Catalog resolution for a component: its record, owner candidates, and the TechDocs runbook excerpt. Needs Backstage configured. | `signalman backstage lookup` |
| `open_incidents` | Incidents currently open in incident.io, the dedup candidate list. | `signalman incidentio open-incidents` |

Every tool's `read_only_hint` annotation is `true`. Every result carries the value as structured content (validated against the tool's declared output where one is fixed, such as `qualify_alert`'s [outcome contract](triage.md#the-outcome-contract)) and a short text summary, for a client that renders content instead of structured JSON. A tool that runs and fails (an unreachable upstream, a missing catalog entry) returns a tool-level error the client shows; a malformed request (both or neither of two mutually exclusive fields) is refused as a protocol-level error before anything runs.

## Configuration

```toml
[mcp]
enabled = true      # a kill switch; signalman mcp still has to be invoked
transport = "stdio" # "http" is accepted but not yet implemented
```

`SIGNALMAN_MCP_ENABLED`, `SIGNALMAN_MCP_TRANSPORT`, `SIGNALMAN_MCP_BIND_ADDRESS` override the file; see [Configuration](configuration.md#mcp). Everything else the tools need — the TypeSafe model, the software catalog, the change feed, the routing thresholds — is the same `[typesafe]`, `[incidentio]`, `[backstage]`, `[flow]`, `[policy]` and `[triage]` configuration the rest of the binary reads. Secrets (`TYPESAFE_API_KEY`, `INCIDENTIO_API_KEY`, `BACKSTAGE_TOKEN`) are environment only, as everywhere else.

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

- **Writing.** No tool tags an alert, writes a note, attaches an incident or notifies an owner. That is a separate, gated tool (`mcp.allow_write`, default off, `signalman-4gp.6`).
- **Streamable HTTP** (`signalman-4gp.7`). `mcp.transport = "http"` is accepted in configuration and fails clearly at start-up naming the bead; it would mount the same tools on the router `serve` already runs, under its own bearer-token secret, not the webhook signing secret (a webhook is incident.io authenticating to signalman, this is the reverse).
- **Investigation or remediation.** A tool answers one typed question; it does not decide what to do next, retry, or chain calls. That is the calling agent's job, which is the point of [decision 0008](decisions/0008-signalman-is-a-tool-for-agents.md).
