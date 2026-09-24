---
title: 0010 Extract the judgment core into a reusable crate
description: The typed question and answer layer over System One models (Jev, Laya) becomes its own crate, a workspace member of this repository first and a published crate when a second consumer exists, so other Rust projects can build on calibrated judgments without carrying the alert-triage application.
status: proposed
date: 2026-09-24
decision-makers: []
consulted: []
informed: []
last_reviewed: 2026-09-24
tags: [decisions, architecture, crate, typesafe]
---

# 0010 Extract the judgment core into a reusable crate

## Context and problem statement

signalman is two things in one package: a typed client for the TypeSafe System One API, and an alert-triage application built on it. The client half is generic. It knows nothing about alerts: it builds questions, returns a typed handle per question, validates the probabilities that come back and reads them through those handles ([TypeSafe client](../typesafe-client.md), [decision 0003](0003-typed-handles-between-questions-and-answers.md)). It has now been exercised against the hosted model and against Laya through a shim ([Laya](../laya.md)), and it carries the retry policy, the recording and replay of raw responses, and the per-question metrics the [evaluation harness](../evaluation.md) is built on.

A second project that wants calibrated judgments from a System One model today has to depend on the whole of signalman: axum, rmcp, the incident.io and Backstage clients, OpenTelemetry, and a configuration layer shaped for one deployment. How do we make the judgment layer a core other Rust projects can build on, without losing what makes it trustworthy here (the gate, the live verification, the tests) and without turning signalman into a library-maintenance project?

Five crates for the same API appeared on crates.io in the week of 16 to 22 September 2026: `typesafe-ai`, `typesafe-client`, `systemone`, `jev` and `system-one`. All are 0.1.x, single-author, between 14 and 116 downloads. Two overlap with this decision directly: `typesafe-client` (typed keys, a `RetryPolicy` with the same SDK defaults as ours, a `SystemOne` transport trait, a `fake` backend, an `http` feature) and `system-one` (a `DecisionBackend` trait with mock, replay, local-logprob and shadow backends, derive macros, cost-matrix decisions, calibration reports). TypeSafe itself publishes SDKs for Python and JavaScript only.

## Decision drivers

- A second consumer must get the typed judgment layer and nothing else: no HTTP server, no incident tooling, no configuration model
- What has been verified live (the wire shape, the retry behaviour, the recordings under `examples/eval/runs/`) must stay verified: the extraction must not change behaviour, and the gate must keep covering it
- One place to fix a contract drift: the client, its tests and its docs move together
- The team is small; a second repository, a release process and semver discipline are costs paid on every change, not once
- TypeSafe may publish an official Rust SDK; the core should be small enough that becoming an adapter over it is a contained change
- Names on crates.io are first come; `typesafe*`, `systemone`, `system-one` and `jev` are taken, and a name containing the vendor's trademark is a liability

## What is generic and what is not

Read from the module dependency graph, not from intent:

| Moves to the core | Lines | Depends on |
|---|---|---|
| `src/question.rs`: `Question`, `NoulCriteria`, `Options`, `options!`, `Handle<A>`, `Questions`, the 255 and 10 limits | 391 | serde, serde_json |
| `src/answer.rs`: `Probability`, `Confidence`, `Answer`, `Usage`, `Response::get`, `FromAnswer`, `Noul`, `Choice<K>`, `Score` | 464 | serde |
| `src/client.rs`: `ClientBuilder`, `Client`, `Request`, `ModelInfo`, `system_one`, `evaluate`, `list_models` | 282 | reqwest, tracing |
| `src/error.rs`: the TypeSafe error enum | 110 | thiserror |
| `src/http.rs`: `RetryPolicy`, `send_with_retries`, `parse_retry_after`, `truncate` | 241 | reqwest, tokio (sleep), fastrand |
| `src/eval/metrics.rs` and the generic half of `src/eval/mod.rs`: `Recording`, recording to and replaying from a directory, per-question `Judgment` grading, accuracy, Brier, ECE bins | ~250 | serde |
| `tests/client.rs` (all but the triage end-to-end case), the unit tests of the modules above, the crate-level doctest | ~230 | wiremock |

| Stays in signalman | Why |
|---|---|
| `src/triage/`: the question set, `Texts`, `Policy`, `decide` | The application's judgments and its routing policy; the pattern every consumer writes for itself |
| `src/outcome.rs`, `src/incidentio/`, `src/backstage/`, `src/changes/`, `src/serve.rs`, `src/mcp/`, `src/readiness.rs`, `src/config.rs`, `src/main.rs` | The product |
| `src/telemetry.rs` | Names signalman's instruments; the core must not (see below) |
| The signalman half of `eval`: `Case`, `Expected`, `Graded`, decision agreement, the CLI | Labels and the policy are the application's |

Two couplings have to be cut, both in the generic modules and both small:

- `client.rs` and `http.rs` call `crate::telemetry::record_typesafe_usage` and `record_upstream_error`. The core gets an `Observer` trait (`on_usage(model, &Usage)`, `on_failed_attempt(service, status)`) with a no-op default, set on the builder; signalman implements it in `src/telemetry.rs`, which keeps the rule that one module names every instrument. Spans stay: the core keeps its `tracing::instrument` attributes, and `tracing` is the one observability dependency a library should have.
- `http.rs` is also used by the incident.io and Backstage clients. It stays public in the core (`RetryPolicy` is part of the client's public surface anyway) and signalman keeps using it from there.

## Considered options

1. Adopt an existing crate and delete our client
2. A workspace member in this repository, consumed by path here and by git dependency elsewhere, published later
3. A separate repository, published on crates.io from the start
4. Keep the single crate and tell other projects to depend on signalman with default features off

## Decision outcome

Chosen option: "A workspace member in this repository, published later", because it gives a second project a dependency this week, keeps one gate and one live-verification history over the code that matters most, and defers the release machinery to the moment a second consumer proves it is needed. The trigger to publish is that consumer, not a date.

Concretely:

- The repository becomes a workspace whose root package stays `signalman`, so no path in `docs/`, `mise.toml`, the workflow or the harness moves. The new member lives at `crates/<name>/`, with `[workspace.package]` for edition, `rust-version` and licence, `[workspace.dependencies]` for shared versions and `[workspace.lints]` for the pedantic set, so both crates are held to the same bar.
- The core's dependency footprint is the wire and nothing else: serde, serde_json, thiserror, tracing, and behind a default `http` feature reqwest with rustls, tokio (for the retry sleep) and fastrand. Without `http`, the questions, answers, recordings and metrics compile alone, for a project that brings its own transport.
- The core gains one trait, `SystemOne`, with a single `evaluate`, implemented by `Client`, by a `Replay` backend over recordings and by a `Fake` for tests. signalman's `Triager` keeps holding a `Client` until a second backend is needed in the product; the trait exists so that an official SDK, a local model behind a different wire, or a shadow backend can be slotted in without touching consumers. This is the one place where the option-1 crates are ahead of us and where the extraction should close the gap.
- The `options!` macro, `Handle<A>`, `Response::get`, the newtypes and the error enum keep their names: they are the interface [decision 0003](0003-typed-handles-between-questions-and-answers.md) argued for, and the crate-level doctest in `src/lib.rs` is the intended experience. Breaking changes are free until the first publish, so the extraction is also the moment to settle what `list_models` is (the endpoint is not in the documented API; it stays, marked as observed rather than documented, because `signalman models` and the readiness probe use it).
- The name is decided before the move so paths do not churn twice. It must not contain `typesafe` or `jev`, and it must be free on crates.io: `judgment`, `judgments`, `calibrated` and `typed-judgment` were free on 2026-09-24. The record is written for `judgment`; the choice is the decision-maker's.
- signalman re-exports the core (`pub use judgment::{Client, Questions, options, ...}`) for one release so nothing downstream of this repository breaks, then drops the re-exports.
- Versioning: `0.1.0` at extraction, path dependency from signalman, git dependency (`rev` or tag) for other projects until publication. Publication (option 3's second half) happens in this repository with `cargo publish -p <name>`: one repository, two crates, the application always built against the version it ships with.

### Consequences

- Good, because a second Rust project depends on a crate with four runtime dependencies instead of the whole application, and gets the typed handles, the validated probabilities, the SDK-equivalent retries and the recording and replay tooling that have already been run against Jev and Laya
- Good, because the client, its tests, its docs and its live-verification recordings stay in one repository under one gate; a contract drift found by `contract-reviewer` is fixed in one place
- Good, because the `Observer` trait removes the last reason a library module reads signalman's telemetry, and the `SystemOne` trait gives the evaluation harness its replay backend for free
- Bad, because a workspace adds a second `Cargo.toml` to keep in step and a second set of docs (rustdoc for the crate; `docs/typesafe-client.md` becomes its README source); `refactor-scout` and `docs-auditor` must learn the second crate
- Bad, because a git dependency is weaker than a published one: no docs.rs, no semver from Cargo's point of view, and a consumer pins a commit. Accepted until a second consumer exists; then publish
- Bad, because five other crates already compete for the same users and an official SDK may arrive; the core is kept small so that either outcome costs an adapter, not a rewrite
- Neutral: signalman's own behaviour does not change; the extraction is validated by the existing test suites passing unchanged, with the same counts

### Confirmation

- `cargo test --workspace` passes with the same test counts before and after; `tests/client.rs` runs from the core crate; the recordings under `examples/eval/runs/` replay to the same report
- `cargo tree -p <name> --edges normal` shows only serde, serde_json, thiserror, tracing, and under `http` reqwest, tokio, fastrand
- `grep -rn 'crate::telemetry' crates/<name>` finds nothing; `src/telemetry.rs` implements `Observer`
- A throwaway binary outside this repository depending on the crate by git compiles the crate-level doctest unchanged

## Pros and cons of the options

### Adopt an existing crate

`typesafe-client` is the closest to ours: the same retry defaults (theirs caps `Retry-After` at 60 s and adds a 30 s overall deadline), typed keys, a transport trait, a fake backend, an `http` feature. `system-one` has the richer idea: pluggable backends including replay and a local-logprob server, cost-matrix decisions instead of thresholds, calibration reports.

- Good, because it deletes 1,500 lines we maintain, and `system-one`'s cost-matrix decisions are a better policy primitive than fixed thresholds
- Bad, because every candidate is between two and eight days old, single-author, 0.1.0, with a live API "exercised by hand" at best; ours has two recorded live runs and a product on top
- Bad, because adopting one means re-verifying the wire and the retry behaviour and rewriting the harness's recordings, to arrive where we are
- Bad, because their APIs will move; being one of a dozen users of a week-old crate is the position this record exists to avoid
- Rejected now. Revisit in six months, or when TypeSafe publishes a Rust SDK: at that point the `SystemOne` trait is the seam, and the candidate becomes a backend, not a replacement

### A workspace member, published later

- Good, because it is the smallest change that yields a dependency another project can use this week
- Good, because one repository keeps one gate, one live-verification history and one harness over the code
- Bad, because a git dependency has no docs.rs and no semver in Cargo's eyes; consumers pin a commit
- Bad, because a second crate needs its own README and rustdoc, and the harness agents need to know it exists

### A separate repository, published from the start

- Good, because discoverability and semver are real from day one, and the boundary cannot erode
- Bad, because CI in this account has never run ([roadmap](../roadmap.md#ci)); a second repository doubles a blocked pipeline, the release process and the pull-request traffic for a team of one
- Bad, because signalman's live verification and the harness would need to be duplicated or lost to the core, and the two repositories drift in Rust version, lint set and conventions
- Bad, because the name must be chosen and published now, into a crowded namespace, before the API has a second consumer to settle it
- Deferred, not rejected: publication from the workspace gives the discoverability without the second repository

### Keep one crate with features

- Good, because nothing moves
- Bad, because default features off still pulls the whole dependency tree into the lockfile and every module into the build; a consumer reads `signalman` in their `Cargo.toml` and wonders what an alert triager is doing there
- Bad, because the coupling to `telemetry` and the unclear boundary stay; features hide a boundary, they do not create one
- Rejected

## More information

The `SystemOne` trait's shape: one method, `evaluate(&self, request: &Request<'_, impl Serialize>) -> impl Future<Output = Result<Response>>`; for dynamic dispatch a boxed-future variant so `Arc<dyn SystemOne>` works, which is what a `Triager` shared across the receiver and the MCP server needs. `Client` implements it; `Replay` (a directory of recordings keyed by case id) and `Fake` (fixed answers per question id, with request inspection) are the two other implementations the harness and the tests need. Sync clients, streaming and batching are not added: the API documents one endpoint and one request shape, and no consumer has asked.

Migration, in the order that keeps the gate green after each step: (1) workspace scaffold, move the five modules and `tests/client.rs`, re-export from signalman, no behaviour change; (2) the `Observer` trait replaces the two telemetry calls; (3) the `SystemOne` trait with `Replay` and `Fake`, and the generic half of `eval` moves; (4) the crate README and a rustdoc pass that explains the why, `docs/typesafe-client.md` becomes a pointer, this record becomes accepted; (5) when a second consumer exists, `cargo publish -p <name>`, a tag and a changelog. Beads for the steps are filed once the name is chosen.
