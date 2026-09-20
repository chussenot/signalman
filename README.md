# rustsafe

A typed Rust client for the [TypeSafe](https://typesafe.ai) System One API,
plus a worked alert-triage application built on it.

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

## What "typed" buys you

- **A question returns a typed handle.** `questions.choice::<Team>(..)` yields
  `Handle<Choice<Team>>`; `response.get(&handle)` returns a `Choice<Team>` whose
  `chosen` field is the enum, not a string. A response of the wrong primitive
  or an unknown option is an error, never a silently misread number.
- **Criteria come from the type.** The `options!` macro defines an enum with its
  wire keys and rubric descriptions in one place; the request's `criteria` map
  is generated from it and the answer is parsed back through it.
- **Probabilities are validated.** `Probability` and `Confidence` are distinct
  newtypes in `[0, 1]`, checked on deserialisation. A confidence is not the
  probability of any outcome, so the compiler keeps them apart.
- **Errors are structured.** 401, 422, 429, 529 and transport failures map to
  variants that carry the attempt count and any `Retry-After`.

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

## The triage example

`src/triage/` applies the docs' recommended shape to alert routing:

1. **Speculative fan-out.** One request asks everything the policy might need:
   owning team (Choice), user impact (Score), whether a human must act (Noul),
   which open incident it duplicates (a dynamic Choice over incident ids plus
   `none`), and whether a listed recent change is the likely cause (Noul).
   Questions whose candidates are empty are not asked: the model cannot pick
   an option it was not offered.
2. **Decide in code.** `policy.rs` turns typed answers into a `Decision`:
   suppress, attach to an existing incident, page, ticket, or hand to a human.
   Thresholds scale with risk: paging needs both a confident owner and a high
   impact; low owner confidence or "none of these" always involves a person.
3. **Tune without re-running inference.** Raw answers are kept; changing a
   threshold in `Policy` changes behaviour with no new API call.

```sh
export TYPESAFE_API_KEY=...            # https://console.typesafe.ai/keys

cargo run -- triage examples/alerts/crashloop.json
cargo run -- triage examples/alerts/dns.json --json
cargo run -- triage examples/alerts/disk-noise.json --print-request   # no API call
cargo run -- models
```

`--print-request` emits the exact body the client would send, so you can paste
it into the [Playground](https://console.typesafe.ai/playground) and compare.

## Client defaults

Mirrors the official SDKs' `RetryPolicy` and constants:

| Setting | Default | Override |
|---|---|---|
| API key | `TYPESAFE_API_KEY` | `Client::builder().api_key(..)` |
| Base URL | `https://api.typesafe.ai` | `TYPESAFE_BASE_URL` |
| Model | `jev-latest` | `TYPESAFE_DEFAULT_MODEL`, `--model` |
| Timeout | 10 s per attempt | `.timeout(..)` |
| Retries | 2, backoff 0.5 s doubling to 5 s, ±25 % jitter | `.retry(RetryPolicy { .. })` |
| Retry on | 408, 429, 5xx (incl. 529), transport errors | |
| `Retry-After` | honoured up to 30 s | `retry_after_max` |

The response's `model` field is the versioned id that answered (for example
`jev-1.13.0`). Log it. Thresholds tuned against one version should be pinned
to that version rather than an alias.

## Develop

```sh
cargo fmt --all --check
cargo clippy --all-targets            # pedantic; CI denies warnings
cargo test                            # unit + wiremock integration + doctests, no network
cargo doc --no-deps --open
```

Tests never call the real API. `tests/client.rs` runs the client against a
`wiremock` server for request shape, error mapping, retry behaviour and an
end-to-end triage. Policy tests build answers by round-tripping fake API
responses through the real handles.

## Layout

```
src/
  client.rs      HTTP, retries, error classification, models listing
  question.rs    Question/Questions builder, Options trait, options! macro, Handle<A>
  answer.rs      Answer wire shape, Probability/Confidence, typed views, Response::get
  error.rs       Error enum
  triage/
    mod.rs       Alert (the state) and OpenIncident
    questions.rs the fan-out and typed handles for it
    policy.rs    Decision and decide()
  main.rs        CLI
examples/alerts/ sample states
.claude/         pins the typesafe skill plugin for this repo
```

## Working with the TypeSafe skill

The repo pins the `typesafe@typesafe-ai` plugin in `.claude/settings.json`.
Its skill sends the agent to the live docs (`https://docs.typesafe.ai/llms.txt`)
before touching questions, answers or the client, which is the right reflex:
the docs are the contract, this crate is one implementation of it.

## Not yet done

- No call against the real API from this environment (no key). The wire
  shape is asserted against the documented examples; expect small drift to
  show up as a `Decode` error, and fix it against the API reference.
- `Retry-After` in HTTP-date form falls back to backoff; only delay-seconds
  is parsed.
- No streaming or batching; one request per evaluation.
