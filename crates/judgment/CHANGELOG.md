# Changelog

All notable changes to the `judgment` crate. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the crate follows
[Semantic Versioning](https://semver.org/) (0.x: a minor bump may break).

## [0.2.0] - Unreleased

Hardens the wire and closes the gaps against the official TypeSafe SDKs that
the [System One client survey](../../docs/research/system-one-client-libraries.md)
identified. TypeSafe has answered this client live once, under signalman's own
account; everything else is checked against wiremock, the published OpenAPI
document (0.2.0) and the SDK references.

### Added

- TypeSafe's `x-typesafe-request-id` is carried on `Response::request_id`, on
  every error that came from an HTTP response (`Error::request_id()`, and a
  ` [request_id …]` suffix on the message), and as the `request_id` field of
  the `typesafe.evaluate` and `typesafe.list_models` spans. It is the last
  attempt's id, and optional everywhere, because the OpenAPI document lists
  no response headers.
- `http::Completed::headers`: the retry loop hands back the last response's
  headers, so each client reads its own upstream's headers.
- `Error::PermissionDenied` for HTTP 403, and `Error::InvalidApiKey` for a key
  the Python SDK (0.7.1) would refuse.
- `ValidationIssue`: the fields a 400 or 422 body names, with a dotted `path()`.
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
  reserved (both SDKs own it) although this release does not send it.
- `Answer::Unknown(Value)` for an answer kind this release does not know,
  logged at `warn`. `Response::extra` keeps undocumented top-level fields.
  (`#[serde(untagged)]` on a variant needs serde 1.0.181 or later.)
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
- A Noul with neither instructions nor criteria is refused by the builder.
  Null instructions are still sent as `null` (the schema accepts it, and Laya
  requires the key).
- Backoff jitter only shortens a wait, as in both SDKs.
- A missing `usage`, or null counts, read as zero.
- A 2xx that does not decode or does not fit the questions is reported to the
  observer as a failed attempt (`decode` or `unfit`) and is not retried.
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
- `Questions::noul` refuses a Noul with neither instructions nor criteria (S2).
- `RetryPolicy` gains public fields, so struct literals need
  `..RetryPolicy::default()` (S3).
- `RetryPolicy::is_retryable` takes `&self` (S3).
- Backoff jitter only shortens a wait, for every client of the shared loop (S3).
- `retry-after-ms` and HTTP-date `Retry-After` are honoured up to
  `retry_after_max` on any retried status, for every client of the shared
  loop (S3).
- New variants `Error::ReservedHeader` and `Error::ReservedField` (S4).
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
  `request_id`; `UnknownOption`'s message is reworded (S6).
- The client refuses a 2xx that does not fit the questions sent, without
  retrying (S6).
- `Observer::on_failed_attempt` also receives `decode` and `unfit` from the
  TypeSafe client (S6).
- `Fake` refuses a scripted answer that does not fit and records no call;
  `Fake::score`'s legend is the question's levels (S6).
- `Replay` refuses a recording that does not fit; `Recorder` writes nothing
  for such a response (S6).
