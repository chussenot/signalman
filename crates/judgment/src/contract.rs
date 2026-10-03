//! The published contract the crate is tested against, for a consumer that
//! validates its own TypeSafe traffic against the same document.
//!
//! [`OPENAPI_DOCUMENT`] is the text of `tests/fixtures/typesafe-openapi.json`,
//! a copy of <https://api.typesafe.ai/openapi.json> (OpenAPI 3.1.0, API
//! version 0.2.0) that `tests/contract.rs` validates every request shape, every
//! `Fake` response and every committed recording against, and that only
//! `tests/openapi_drift.rs` refreshes, written canonically (sorted keys, a
//! final newline). It sits behind the `openapi` feature because it is about
//! 25 KB of static data that a client has no use for at run time. An
//! application that checks its own requests and mocks against the contract,
//! as signalman does in its `tests/typesafe_contract.rs`, turns the feature on
//! in its dev-dependencies and parses the text with `serde_json`; that is the
//! one copy of the document, so the application's checks and the crate's
//! cannot drift apart.
//!
//! The document is the schema TypeSafe publishes, not what the crate enforces.
//! Where the crate is stricter (the HTTP API reference page's limits on
//! options and levels) or more tolerant (a missing `usage`) is pinned, at its
//! own path, in `tests/contract.rs`.

/// The vendored TypeSafe System One OpenAPI document, as JSON text.
pub const OPENAPI_DOCUMENT: &str = include_str!("../tests/fixtures/typesafe-openapi.json");
