---
title: TypeSafe client
description: Why signalman's TypeSafe client is the separate judgment crate, where the crate's own documentation lives, and how signalman configures it, observes it and builds its triage on it.
status: current
last_reviewed: 2026-10-04
tags: [typesafe, library, judgment]
---

# TypeSafe client

A second project that wanted calibrated judgments from a TypeSafe System One model had to depend on the whole triager: axum, rmcp, two other API clients, OpenTelemetry and a configuration model shaped for one deployment. [Decision 0010](decisions/0010-extract-the-judgment-core-into-a-crate.md) moved the typed client into its own crate, `judgment`, first as a member of this workspace; [decision 0012](decisions/0012-the-judgment-crate-moves-to-its-own-repository.md) moved the crate to its own repository, [chussenot/judgment](https://github.com/chussenot/judgment), with its history. signalman depends on it from [crates.io](https://crates.io/crates/judgment), `judgment = { version = "0.5", features = ["jud"] }` in `Cargo.toml` since 0.5 (`"0.3"` when it was first published, by git and pinned to a revision for the first day), and re-exports it; the crate's rustdoc is on [docs.rs](https://docs.rs/judgment).

This page covers what signalman does with the crate. The crate documents itself, in its own folder, because it is meant to be read and published without this application ([decision 0011](decisions/0011-documentation-lives-with-its-concern.md)):

| To learn | Read |
|---|---|
| What the crate guarantees, the walkthrough, how to depend on it | [the crate README](https://github.com/chussenot/judgment/blob/main/README.md) |
| How typed handles, the response check and the retry loop work | [How judgment works](https://github.com/chussenot/judgment/blob/main/docs/design.md) |
| Which crate types carry TypeSafe's four patterns, with a runnable example each | [the README's Patterns section](https://github.com/chussenot/judgment/blob/main/README.md#patterns) |
| What the crate does against the hosted API and against Laya | [Verification](https://github.com/chussenot/judgment/blob/main/docs/index.md#verification) |
| Every page, as an agent reads it | [the crate's llms.txt](https://github.com/chussenot/judgment/blob/main/docs/llms.txt) |
| Every type, error and default | the rustdoc: `cargo doc -p judgment --open` (no docs.rs page until the crate is published) |

The wire contract is the [live TypeSafe documentation](https://docs.typesafe.ai/llms.txt) and the [OpenAPI document](https://api.typesafe.ai/openapi.json) beside it. The crate vendors that document and contract-tests every request it builds against it (its `tests/contract.rs`). signalman's own triage requests and TypeSafe mocks are checked against the same document by `tests/typesafe_contract.rs`.

## How signalman uses it

- `signalman::Client` is a re-export of `judgment::Client`, the same type. The re-exports exist so nothing built against signalman breaks during the extraction, and are dropped after one release.
- The binary builds the client from `typesafe.base_url`, `typesafe.model` and `typesafe.timeout_seconds` ([Configuration](configuration.md)). The key comes from `TYPESAFE_API_KEY` and nowhere else. Any server that speaks the System One wire works, which is how [Laya](laya.md) was run with no code change.
- signalman implements `judgment::Observer` in `src/telemetry.rs` and installs it as the process-wide observer in `Providers::init`, before the first client exists. That is why the crate's token usage and failed attempts appear as `signalman.typesafe.tokens` and `signalman.upstream.errors` ([Observability](observability.md#metrics)) while the crate names no instrument. A response that does not fit the questions it answers is counted there as `status="unfit"`, and one whose body does not decode as `decode`.
- The incident.io and Backstage clients call `judgment::http::send_with_retries` with their own service label, so all three upstreams retry the same way and every failed attempt is counted once. signalman uses the default retry policy for all three and sets no overall budget; in `serve`, each triage's own deadline bounds it instead ([Operations](operations.md#backpressure)).
- signalman sets no per-call options and no default headers. Every call is `Client::system_one` (or `Client::evaluate` in `signalman triage`, which can print the request it sends) with the configured timeout, the default retry policy and `typesafe.model` as the model.
- The crate refuses a response that does not fit before signalman reads it, and `TriageQuestions::read` verifies again for a recording replayed by case id. So an owner or an incident the model was never offered fails the triage instead of being read as the no-match option ([Triage](triage.md#which-questions-are-asked)).
- TypeSafe's request id reaches signalman's logs and spans through the crate: on every error message and on the `typesafe.evaluate` span ([Observability](observability.md#spans)). Quote it to TypeSafe support when a triage fails on the model call.
- The triage question set (`src/triage/questions.rs`) is the pattern any consumer writes: the crate's primitives are code, and the wording is a `.jud` rubric (`src/triage/triage.jud`, read through `judgment::jud`, feature `jud`) that can be tuned without touching the handles ([Triage](triage.md#the-rubric)). In the terms of TypeSafe's patterns it is a speculative fan-out with a confidence gate on top ([the crate's Patterns section](https://github.com/chussenot/judgment/blob/main/README.md#patterns)).
- The [evaluation harness](evaluation.md) grades through `judgment::eval`. Recordings, per-question grading and the calibration metrics are the crate's; the labels and the decision are signalman's.

## What signalman does not use yet

The crate has a `SystemOne` trait with `Fake`, `Recorder` and `Replay` behind it, but signalman's `Triager` holds a concrete `Client` rather than a `dyn SystemOne`. The trait is the seam an official SDK or a self-hosted model would plug into; it is not threaded through the triage until a second backend is needed in production. What the crate itself leaves out, and why, is on [How judgment works](https://github.com/chussenot/judgment/blob/main/docs/design.md#what-the-crate-does-not-do).
