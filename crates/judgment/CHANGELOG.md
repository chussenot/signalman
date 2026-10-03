# Changelog

All notable changes to the `judgment` crate. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the crate follows
[Semantic Versioning](https://semver.org/) (0.x: a minor bump may break).

## [Unreleased]

### Added

- One runnable example per TypeSafe pattern, written to the documentation
  page's own scenario and thresholds: `fan_out`, `confidence_routing`,
  `composite_scoring` and `intent_routing`. Each replays `jev-1.13.0`'s
  recorded answers by default, calls the API with `--live` or `--record`,
  and carries a test over its recordings that `cargo test` runs. A
  "Patterns" section in the README and the crate docs says which types
  carry each shape.
- `Error::InvalidRequest::kind`: the server's machine-readable `error_type`
  when a 400 body carries one (`api_usage_error`, `max_tokens_exceeded`,
  observed on the hosted API 2026-10-03), and `Error::is_request_too_large`
  for the refusals whose remedy is a smaller request: that 400 when its code
  is `max_tokens_exceeded`, and a 413, which `laya-serve` answers for a body
  past one of its own limits (0.3.24: a state over 50,000 characters, more
  than 64 questions, 100 options or 32 levels, a body over 2 MiB). A 400
  whose body has an `error_type` and no message now reads that code as its
  detail instead of the raw body.
- `Choice::confidence_from_probabilities`, `Score::expected_value` and
  `Score::confidence_from_probabilities`: the formulas TypeSafe documents,
  computed from the wire's probabilities. On the hosted API they agree with
  the wire's `confidence` and `score` within rounding; against a server that
  defines confidence otherwise (Laya) the difference is now a number a
  caller can log.
- The builder refuses an empty question id (the hosted API refuses it with a
  400) and an empty Choice option key (the hosted API accepts it and can
  choose it).
- Live tests for the server's own limits, the stability of repeated calls
  (the decision holds; probabilities moved by up to 0.05 between identical
  clear-cut requests and by 0.19 on an ambiguous one), the confidence formulas and the token budget
  (`tests/live.rs`); `fixtures/models.json` is the hosted API's list as
  served (two aliases, RFC 3339 release dates), no longer a guess from the
  schema. All fifteen pass against `laya-serve` 0.3.24 as well, with its
  full payload and with `LAYA_JEV_STRICT=1` behind a bearer key, and
  against the shim now at `examples/laya/serve_laya.py` (moved from the
  repository's `examples/`), on `laya` 0.3.24
  (`docs/verification/laya-typed-decisions.md`).
- `eval::metrics::wilson_interval` and `QuestionMetrics::accuracy_interval95`:
  a 95% Wilson interval beside every accuracy, because a ratio on three
  labelled cases and one on three hundred read the same without it. A
  consumer that builds `QuestionMetrics` by struct literal gains a field.
- `eval::fingerprint(&Value)`: the canonical FNV-1a hash that keys a
  recording, over any JSON value, so a harness can name the question texts
  or option sets a run was recorded under and refuse to grade old answers
  under new questions.
- `RetryPolicy::max_body_bytes` (default 8 MiB) caps what the shared retry
  loop buffers of a response body: a `Content-Length` over it fails before a
  byte is read, a body without one is read until it passes the cap. The
  failure is `Error::ResponseTooLarge { limit }`, never retried, reported to
  the observer as `too_large`.

### Changed

- `Response::verify` accepts a structured Score level echoed as any string
  that parses to the level sent, not only as its compact JSON. `laya-serve`
  0.3.22 and later echo the JSON text they showed the model, with Python's
  `", "` and `": "` separators (0.3.20 echoed the value, as the hosted API
  does), and every structured level failed `verify` against them. Spacing
  and key order no longer matter; a number written differently (`1.0` for
  `1`) and a string level echoed as anything but itself still do.
  `Score::levels` labels a level echoed as text with that text, and one
  echoed as a value with its compact JSON, as before.
- A 413 is `Error::InvalidRequest { status: 413, .. }`, with the server's
  `detail` and no retry, where it was `Error::Http`; `laya-serve` answers it
  for a body past one of its limits, and the hosted API never sends one.
- `Error::InvalidRequest` has a new field, `kind`; a struct literal or a
  pattern that names every field needs `kind` or `..`.
- `http::Exhausted` is an enum, `Transport { attempts, source }` beside the
  new `TooLarge { attempts, limit }`; a caller that destructured the struct
  matches the first variant instead.
- Bodies are decoded as UTF-8 with invalid sequences replaced, no longer by
  the `Content-Type` charset; every upstream answers in JSON.

## [0.2.0] - 2026-09-25

Hardens the wire and closes the gaps against the official TypeSafe SDKs that
the [System One client survey](docs/research/system-one-client-libraries.md)
identified. TypeSafe has answered this client live once, under signalman's own
account; everything else is checked against wiremock, the published OpenAPI
document (0.2.0) and the SDK references.

### Added

- TypeSafe's `x-typesafe-request-id` is carried on `Response::request_id`, on
  every error that came from an HTTP response (`Error::request_id()`, and a
  ` [request_id …]` suffix on the message), and as the `request_id` field of
  the `typesafe.evaluate` and `typesafe.list_models` spans. It is the last
  attempt's id, and optional everywhere, because the OpenAPI document lists
  no response headers. A value that is empty, longer than 256 bytes or not
  printable ASCII (a tab inside it included) is ignored. A body `request_id`
  that is not a string (the documented body has no such field) reads as
  `None` instead of failing the response.
- `http::Completed::headers`: the retry loop hands back the last response's
  headers, so each client reads its own upstream's headers.
- `Error::PermissionDenied` for HTTP 403, and `Error::InvalidApiKey` for a key
  the Python SDK (0.7.1) would refuse.
- `ValidationIssue`: the fields a 400 or 422 body names, with a dotted `path()`.
  It is `#[non_exhaustive]`, so fields such as `ctx` can be added in a minor
  release; its fields are public to read.
- `RetryPolicy::conservative()`: retries only 408, 429 and failures before the
  request left the process, for callers who would rather fail a billed call
  than pay for it twice.
- `RetryPolicy::{http_statuses, transport, budget}` and `TransportRetry`. The
  total retry budget is off by default. A 2xx is never retried, even when
  listed in `http_statuses`, as in both SDKs: a success is not sent again.
- `retry-after-ms` and the HTTP-date form of `Retry-After` are honoured, up to
  `retry_after_max`. A date is measured against the response's `Date` header.
- `Client::evaluate_with(&Request, &CallOptions)`, for a per-call timeout,
  retry policy, headers and extra body fields. Also `ClientBuilder::default_header`,
  `Client::model()` and `Client::retry()`. `x-typesafe-retry-count` is
  reserved (both SDKs own it) although this release does not send it. The
  headers HTTP owns (`content-length`, `transfer-encoding`, `host`,
  `connection`, `te`, `upgrade`) are refused too: the HTTP stack would keep a
  caller's value over its own, truncating or reframing the body, or sending
  the key to another virtual host than the base URL names.
- `Answer::Unknown(Value)` for an answer kind this release does not know,
  logged at `warn` with the answer's key and kind, both escaped and cut to 64
  characters because the server chose them. `Response::extra` keeps
  undocumented top-level fields. (`#[serde(untagged)]` on a variant needs
  serde 1.0.181 or later.)
- `Response::verify(&Questions)`, `Question::kind()` and `Error::is_unfit()`.
  Every backend in the crate (Client, Fake, Replay, Recorder) returns only a
  response that answers the questions it was sent. A structured Score level
  may be echoed as itself or as its compact JSON.
- Contract tests against the vendored `api.typesafe.ai/openapi.json`
  (`tests/contract.rs`), covering requests, Fakes and recordings, plus an
  ignored drift test (`tests/openapi_drift.rs`).

### Changed

- A 400 is `Error::InvalidRequest { status: 400, .. }`, like 422, and its
  `detail` is the server's message or the parsed issues rather than the raw
  body. Echoed request `input` is dropped from parsed issues.
- The API key is trimmed (a trailing newline from a key file is fine) and
  validated when the client is built. A blank explicit key never falls back to
  `TYPESAFE_API_KEY`.
- A Noul with neither instructions nor criteria is refused by the builder;
  criteria that describe neither outcome (`{}`, or both sides null) count as
  none. Null instructions are still sent as `null` (the schema accepts it, and
  Laya requires the key).
- Backoff jitter only shortens a wait, as in both SDKs.
- The client follows no redirect, like the Python SDK and unlike the JS SDK:
  a 3xx is `Error::Http` with its status, not retried. 0.1 followed up to
  ten, re-sending the body (the state included) on a 307 or 308; with 0.2's
  default and per-call headers it would also have sent a gateway credential
  to whatever origin the redirect named, since only `authorization` is
  stripped across origins. A moved API is a base URL to change.
- A missing `usage`, or null counts, read as zero.
- A 2xx that does not decode or does not fit the questions is reported to the
  observer as a failed attempt (`decode` or `unfit`) and is not retried.
- `Error::Http` carries the attempt count and its message names it (`unexpected
  HTTP status 503 after 3 attempts: …`), so a 5xx the policy retried reads
  apart from one it returned at once; an empty or blank body reads `no body`
  in the message, while the `body` field keeps what the server sent.
- Server-chosen strings in error messages are escaped and cut to 64
  characters, as an unknown answer kind is: the off-list option in
  `Error::UnknownOption`'s message (the `option` field keeps it whole) and a
  probability key in a Score's `Error::InvalidAnswer` reason. An answer sent
  as a JSON string is named by its type in the `Error::Decode` message, not
  quoted.
- `Fake::score` echoes the question's levels as its legend. A Fake refuses a
  scripted answer that does not fit its question.
- The rustdoc states where retries match the SDKs and where they deliberately
  differ, instead of claiming to mirror them. The `/v1/models` notes and the
  question-limit notes are corrected against the OpenAPI document.

### Fixed

- A `Retry-After` of `inf`, or a value of `1e20` or more, panicked in the retry
  loop. Such values are now ignored.
- A malformed API key is no longer reported as a URL error.

### Breaking changes

- `Error` is `#[non_exhaustive]` (S1).
- `Error::Unauthorized` is a struct variant with `request_id` (S1).
- `Error::RateLimited`, `Overloaded` and `Http` gain `request_id` (S1).
- `Error::Http` gains `attempts`, and its message names the attempt count and
  reads `no body` for an empty body (S1).
- `Error::Decode` is a struct variant `{ source, request_id }` with a
  `From<serde_json::Error>` impl (S1).
- `http::Completed` is `#[non_exhaustive]` with a new `headers` field (S1).
- `Response` gains the public field `request_id` (S1).
- `Error::InvalidRequest` gains `status`, `issues` and `request_id`, covers
  400, and its `detail` is a summary, not the raw body (S1, S2).
- A 403 is `Error::PermissionDenied`, not `Error::Http` (S2).
- A 400 is `Error::InvalidRequest { status: 400, .. }`, not `Error::Http` (S2).
- New `Error::InvalidApiKey`: a key with inner whitespace, a control character
  or a non-ASCII character (a BOM included) is refused at `build()` (S2).
- A non-UTF-8 `TYPESAFE_API_KEY` is `InvalidApiKey`, not `MissingApiKey` (S2).
- The key is trimmed; a blank explicit key is `MissingApiKey` and never falls
  back to the environment; the `MissingApiKey` message is reworded (S2).
- `Questions::noul` refuses a Noul with neither instructions nor criteria;
  criteria that describe neither outcome count as none (S2).
- `RetryPolicy` gains public fields, so struct literals need
  `..RetryPolicy::default()` (S3).
- `RetryPolicy::is_retryable` takes `&self` (S3).
- Backoff jitter only shortens a wait, for every client of the shared loop (S3).
- `retry-after-ms` and HTTP-date `Retry-After` are honoured up to
  `retry_after_max` on any retried status, for every client of the shared
  loop (S3).
- New variants `Error::ReservedHeader` and `Error::ReservedField` (S4).
- A 3xx is `Error::Http` instead of being followed (S4).
- `Answer` gains `Unknown`, is `#[non_exhaustive]`, and its hand-written
  `Deserialize` accepts unknown kinds (S5).
- `Answer::kind` returns `&str` and is no longer `const` (S5).
- `Error::AnswerTypeMismatch.actual` is a `String` (S5).
- `Response` gains the public field `extra` and no longer decodes from a JSON
  array (S5).
- A response with no `usage`, or null counts, decodes with zero instead of
  failing (S5).
- `Error::MissingAnswer` is a struct variant `{ id, request_id }` (S6).
- `Error::UnknownOption`, `InvalidAnswer` and `AnswerTypeMismatch` gain
  `request_id`; `UnknownOption`'s message is reworded, and quotes the option
  escaped and cut to 64 characters (S6).
- The client refuses a 2xx that does not fit the questions sent, without
  retrying (S6).
- `Observer::on_failed_attempt` also receives `decode` and `unfit` from the
  TypeSafe client (S6).
- `Fake` refuses a scripted answer that does not fit and records no call;
  `Fake::score`'s legend is the question's levels (S6).
- `Replay` refuses a recording that does not fit; `Recorder` writes nothing
  for such a response (S6).
- Version 0.1.0 becomes 0.2.0 (S8).
