# rustsafe

A typed Rust client for the [TypeSafe](https://typesafe.ai) System One API,
an alert-triage flow built on it, and an [incident.io](https://incident.io)
integration that puts the judgments where responders already work.

TypeSafe's model, **Jev**, does not generate text. It evaluates a `state` (any
JSON) against typed questions and returns calibrated judgments your code can
branch on:

| Primitive | Question | Answer |
|---|---|---|
| Noul | yes / no | probability of yes |
| Choice | one of a defined set | chosen option, full distribution, confidence |
| Score | degree on ordered levels | weighted position, per-level distribution, confidence |

There is no official Rust SDK (Python and JavaScript only). This crate covers
the HTTP contract with the same defaults and retry policy as the official SDKs,
and adds a typed layer so the Rust side and the API contract cannot drift.

## How it fits together

```
 Datadog / Alertmanager / ...          ┌──────────────────────────┐
            │  alerts                   │        TypeSafe          │
            ▼                           │  POST /v1/systemone      │
 ┌──────────────────────┐   webhook     │  owner? impact? action-  │
 │      incident.io     │ ────────────▶ │  able? duplicate of?     │
 │  alerts · incidents  │  alert_created│  caused by change?       │
 │  alert routes        │ ◀──────────── └──────────────────────────┘
 │  escalations         │  tags + attach          ▲
 └──────────────────────┘                          │ one request
            ▲                                      │
            │  enriched alert event        ┌───────┴────────┐
            └──────────────────────────────│    rustsafe    │
               (CLI, optional)             │ serve · triage │
                                           └────────────────┘
```

incident.io stays the alert hub. Alerts land there first from every source.
When one is created, incident.io sends a webhook; `rustsafe serve` verifies it,
fetches the alert and the live incidents fresh from the API (the webhook docs'
rule for staying in sync), asks TypeSafe one fan-out request, decides in code,
and writes back:

- **tags** on the alert: `ai-team-<team>`, `ai-impact-<level>`,
  `ai-action-<decision>`, `ai-dup-<incident ref>`, `ai-suspected-change`.
  Alert routes can filter and escalate on them.
- **an attachment** to the existing incident when the model is confident the
  alert is a duplicate (`POST /v2/incident_alerts`).

Incidents are never created directly: incident.io's alert routes own that
decision, and its 10 incidents/hour API limit stays untouched.

## What "typed" buys you

- **A question returns a typed handle.** `questions.choice::<Team>(..)` yields
  `Handle<Choice<Team>>`; `response.get(&handle)` returns a `Choice<Team>` whose
  `chosen` field is the enum, not a string. A response of the wrong primitive
  or an unknown option is an error, never a silently misread number.
- **Criteria come from the type.** The `options!` macro defines an enum with its
  wire keys and rubric descriptions in one place; the request's `criteria` map
  is generated from it and the answer is parsed back through it.
- **Probabilities are validated.** `Probability` and `Confidence` are distinct
  newtypes in `[0, 1]`, checked on deserialisation.
- **Errors are structured.** Both clients map status codes to variants that
  carry attempt counts, `Retry-After`, and for incident.io the `request_id` and
  field-level validation messages from the documented error body.

```rust
use rustsafe::{Client, Questions, options};

options! {
    enum Department {
        Billing = "billing" => "Payments, invoicing, refunds",
        Technical = "technical" => "Bugs, outages, integrations",
        Sales = "sales" => "Pricing, upgrades, new accounts",
    }
}

let mut questions = Questions::new();
let dept = questions.choice::<Department>("department", "Which team should handle `message`?")?;
let urgent = questions.noul("is_urgent", "Does `message` convey urgency?", None)?;

let client = Client::from_env()?;                    // TYPESAFE_API_KEY
let state = serde_json::json!({ "message": "Help! My payouts have been failing for 3 days." });
let response = client.system_one(&state, &questions).await?;

let dept = response.get(&dept)?;                     // Choice<Department>
let urgent = response.get(&urgent)?;                 // Noul
if dept.chosen == Department::Billing && dept.confidence.at_least(0.7) && urgent.is_yes(0.6) {
    page_billing_oncall();
}
```

## The triage flow

`src/triage/` applies the docs' recommended shape:

1. **Speculative fan-out.** One request asks everything the policy might need:
   owning team (Choice), user impact (Score), whether a human must act (Noul),
   which open incident it duplicates (a dynamic Choice over incident references
   plus `none`), and whether a listed recent change is the likely cause (Noul).
   Questions whose candidates are empty are not asked: the model cannot pick
   an option it was not offered.
2. **Decide in code.** `policy.rs` turns typed answers into a `Decision`:
   suppress, attach to an existing incident, page, ticket, or hand to a human.
   Thresholds scale with risk: paging needs both a confident owner and a high
   impact; low owner confidence or "none of these" always involves a person.
3. **Tune without re-running inference.** Raw answers are kept; changing a
   threshold in `Policy` changes behaviour with no new API call.

## Setup

```sh
cp .env.example .env   # fill in keys; never commit it
```

| Variable | Used by | Where to get it |
|---|---|---|
| `TYPESAFE_API_KEY` | everything | console.typesafe.ai/keys |
| `INCIDENTIO_API_KEY` | `serve`, `incidentio *`, `--dedup-from-incidentio` | Settings → API keys. Scopes: view alerts and incidents, manage alert tags, manage incident alerts |
| `INCIDENTIO_WEBHOOK_SECRET` | `serve` | Settings → Webhooks → your endpoint → Signing secret (`whsec_...`) |
| `INCIDENTIO_ALERT_SOURCE_CONFIG_ID`, `INCIDENTIO_ALERT_SOURCE_TOKEN` | `--forward-to-incidentio` | an HTTP alert source's id and secret token |

### Webhook receiver

```sh
cargo run -- serve --addr 0.0.0.0:8080            # POST /webhooks/incidentio, GET /healthz
cargo run -- serve --dry-run                       # decide, write nothing back
```

In incident.io, Settings → Webhooks → add endpoint → subscribe to
**Alert created (public)** (`public_alert.alert_created_v1`) → copy the
signing secret. Deliveries from other event types are acknowledged and ignored.
Non-2xx responses are retried by incident.io for 24 hours, so the endpoint only
rejects what it cannot accept: bad signature (401) or an unparseable body (400).
Duplicate deliveries (same `webhook-id`) are acknowledged without work.

To exercise the flow without a webhook:

```sh
cargo run -- incidentio whoami
cargo run -- incidentio open-incidents
cargo run -- incidentio triage-alert 01GW2G3V0S59R238FAHPDS1R66 --dry-run
```

### CLI triage

```sh
cargo run -- triage examples/alerts/crashloop.json
cargo run -- triage examples/alerts/dns.json --dedup-from-incidentio --json
cargo run -- triage examples/alerts/disk-noise.json --print-request      # no API call
cargo run -- triage examples/alerts/dns.json --forward-to-incidentio     # → HTTP alert source
```

`--forward-to-incidentio` posts the alert with the judgments under
`metadata.ai` to an HTTP alert source. Map `metadata.ai.team`,
`metadata.ai.impact_label`, `metadata.ai.decision.action` to alert attributes
in the source's template, then route on them.

## Client defaults

Both clients share one retry loop. TypeSafe defaults mirror the official SDKs:

| Setting | Default | Override |
|---|---|---|
| Timeout | 10 s (TypeSafe), 15 s (incident.io) | `.timeout(..)` |
| Retries | 2, backoff 0.5 s doubling to 5 s, ±25 % jitter | `.retry(RetryPolicy { .. })` |
| Retry on | 408, 429, 5xx (incl. 529), transport errors | |
| `Retry-After` | honoured up to 30 s | `retry_after_max` |

The TypeSafe response's `model` field is the versioned id that answered. Log
it; thresholds tuned against one version should be pinned to that version.
The incident.io list-incidents endpoint has its own 60/min limit; dedup
candidates are capped at 40 per triage (`Triager::max_candidates`).

## Develop

```sh
cargo fmt --all --check
cargo clippy --all-targets            # pedantic; CI denies warnings
cargo test                            # unit + wiremock integration + doctests, no network
cargo doc --no-deps --open
```

Tests never call a real API. `tests/client.rs` and `tests/incidentio.rs`
exercise each client against `wiremock`; `tests/webhook_server.rs` runs the
whole flow: a Svix-signed webhook through the router, mock incident.io and
mock TypeSafe, asserting the tags and the attachment. The signature verifier
is pinned to Svix's published test vector.

## Layout

```
src/
  client.rs        TypeSafe HTTP client
  http.rs          shared retry loop, backoff, Retry-After
  question.rs      Question/Questions builder, Options trait, options! macro, Handle<A>
  answer.rs        Answer wire shape, Probability/Confidence, typed views, Response::get
  triage/          Alert state, fan-out questions, Decision policy
  incidentio/
    client.rs      incidents, alerts, tags, incident_alerts, alert events, identity
    types.rs       wire types (lenient: unknown fields ignored)
    webhook.rs     Svix signature verification, event envelope parsing
    sync.rs        Triager: fetch → judge → decide → write back
  serve.rs         axum webhook receiver
  main.rs          CLI: triage, models, serve, incidentio {whoami, open-incidents, triage-alert}
examples/alerts/   sample states     examples/webhooks/  sample delivery
.claude/           pins the typesafe skill plugin
```

## Not yet done

- No call against the real TypeSafe or incident.io APIs from this environment
  (no keys). Wire shapes are asserted against the documented examples and the
  OpenAPI spec; drift shows up as a `Decode` error naming the field.
- Multiple values for one incident.io list filter are sent as a repeated
  `status_category[one_of]` key. The docs show single values only; verify
  against a real account and fall back to three requests if needed.
- `public_incident.incident_created_v2` is parsed but not acted on. A natural
  next step: judge severity from the incident summary and post it as a timeline
  note for the lead to confirm.
- Alert `resolved` events are not forwarded by the CLI.
