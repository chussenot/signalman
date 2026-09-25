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

### Breaking changes

- `Error` is `#[non_exhaustive]` (S1).
- `Error::Unauthorized` is a struct variant with `request_id` (S1).
- `Error::RateLimited`, `Overloaded` and `Http` gain `request_id` (S1).
- `Error::Decode` is a struct variant `{ source, request_id }` with a
  `From<serde_json::Error>` impl (S1).
- `http::Completed` is `#[non_exhaustive]` with a new `headers` field (S1).
- `Response` gains the public field `request_id` (S1).
- `Error::InvalidRequest` gains `request_id` (S1).
