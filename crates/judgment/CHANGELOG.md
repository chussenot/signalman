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
- The `/v1/models` notes are corrected against the OpenAPI document.

### Fixed

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
