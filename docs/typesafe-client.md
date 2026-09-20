---
title: TypeSafe client
description: How the Rust client covers the TypeSafe System One HTTP contract and adds compile-time typing between questions and answers.
status: current
last_reviewed: 2026-09-20
tags: [typesafe, library]
---

# TypeSafe client

TypeSafe's model, Jev, evaluates a `state` (any JSON) against typed questions and returns calibrated judgments. The [live documentation](https://docs.typesafe.ai/llms.txt) is the contract; this page describes how the crate implements it.

| Primitive | Question | Answer |
|---|---|---|
| Noul | yes or no | probability of yes |
| Choice | one of a defined set | chosen option, full distribution, confidence |
| Score | degree on ordered levels | weighted position, per-level distribution, confidence |

There is no official Rust SDK. The client mirrors the Python SDK's defaults so behaviour matches across languages.

## Typed handles

Adding a question returns a `Handle<A>` whose type parameter is the answer type. Reading through the handle checks the primitive and converts.

```rust
use signalman::{Client, Questions, options};

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

let client = Client::from_env()?;
let state = serde_json::json!({ "message": "Help! My payouts have been failing for 3 days." });
let response = client.system_one(&state, &questions).await?;

let dept = response.get(&dept)?;      // Choice<Department>
let urgent = response.get(&urgent)?;  // Noul
```

What this rules out:

- Reading a Score where a Noul was asked. `AnswerTypeMismatch` names the id and both primitives.
- An option string the enum does not know. `UnknownOption` instead of a silent default.
- A probability outside `[0, 1]`. `Probability` and `Confidence` validate on deserialisation and are distinct types, because a confidence is not the probability of any outcome.

The `options!` macro writes the `Options` implementation: the wire keys, the rubric text sent as `criteria`, and the reverse lookup. Keys and descriptions live in one place, so the request and the parser cannot disagree.

For candidate sets only known at runtime, `Questions::dynamic_choice` takes `(key, description)` pairs and returns `Handle<Choice<String>>`.

## Building questions

`Questions::noul`, `choice`, `dynamic_choice` and `score` validate what the API would reject: duplicate ids, fewer than two options, more than 255 options, fewer than two or more than ten levels. Instructions accept any `serde_json::Value`, so structured instructions with a `question` field and reference data are supported as the docs recommend.

## Defaults

| Setting | Default | Override |
|---|---|---|
| API key | `TYPESAFE_API_KEY` | `Client::builder().api_key(..)` |
| Base URL | `https://api.typesafe.ai` | `TYPESAFE_BASE_URL` |
| Model | `jev-latest` | `TYPESAFE_DEFAULT_MODEL`, `--model` |
| Timeout | 10 s per attempt | `.timeout(..)` |
| Retries | 2; backoff 0.5 s doubling to 5 s; ±25 % jitter | `.retry(RetryPolicy { .. })` |
| Retry on | 408, 429, 5xx (including 529), transport errors | |
| `Retry-After` | honoured up to 30 s, delay-seconds form only | `retry_after_max` |

The response's `model` field is the versioned id that answered, for example `jev-1.13.0`, even when an alias was requested. It is logged and returned in every outcome. Thresholds tuned against one version should pin that version.

## Errors

`signalman::Error` distinguishes `MissingApiKey`, `Unauthorized` (401), `InvalidRequest` (422 with the body), `RateLimited` and `Overloaded` (429 and 529 after retries, with attempt counts), `Http`, `Transport`, and `Decode`, plus the typed-layer errors above. The API key is marked sensitive and redacted from `Debug` output.
