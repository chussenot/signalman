//! The vendored OpenAPI document against the one TypeSafe serves today.
//!
//! `tests/contract.rs` checks every request the crate builds, every Fake
//! response and every committed recording against
//! `tests/fixtures/typesafe-openapi.json`, a copy of
//! <https://api.typesafe.ai/openapi.json>. A copy keeps the default test run
//! offline and makes a change to the published contract a reviewed diff
//! rather than a test that starts failing on its own one morning. The cost is
//! that the copy can go stale, and this test is how to find out: it fetches
//! the live document and compares the two as JSON values.
//!
//! It is `#[ignore]` because it needs the network (no key: the document is
//! public), so the gate never runs it. By hand:
//!
//! ```sh
//! cargo test -p judgment --test openapi_drift -- --ignored
//! ```
//!
//! With `JUDGMENT_OPENAPI_WRITE` set it rewrites the fixture from the live
//! document instead of comparing, which is the only way the fixture is ever
//! written (a missing fixture reads as null, so the first run creates it):
//!
//! ```sh
//! JUDGMENT_OPENAPI_WRITE=1 cargo test -p judgment --test openapi_drift -- --ignored
//! cargo test -p judgment --test contract
//! ```
//!
//! The fixture is written canonically, as `serde_json::to_string_pretty`
//! with sorted keys and a final newline, so a refresh's diff is the change in
//! the contract and nothing else. `tests/contract.rs` checks the bytes are
//! canonical, which catches a hand edit that reformats the file; a hand edit
//! to the content passes offline and shows up only when this test is run
//! against the live document. Review a refresh by rerunning the
//! contract test: a gap it pins (something the crate sends that the schema
//! refuses, or the reverse) fails at its own path when the new document
//! closes it.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::print_stderr)]

use std::collections::BTreeSet;
use std::path::PathBuf;

use serde_json::Value;

/// Where TypeSafe publishes the document.
const LIVE_URL: &str = "https://api.typesafe.ai/openapi.json";

/// Set to rewrite the fixture from the live document.
const WRITE_VAR: &str = "JUDGMENT_OPENAPI_WRITE";

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/typesafe-openapi.json")
}

/// The canonical bytes of a document: what the fixture holds.
fn canonical(document: &Value) -> String {
    serde_json::to_string_pretty(document).unwrap() + "\n"
}

/// The keys of the object at `pointer` in either document whose values
/// differ between them, a key present in only one included.
fn differing_keys(vendored: &Value, live: &Value, pointer: &str) -> BTreeSet<String> {
    let object = |document: &Value| {
        document
            .pointer(pointer)
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default()
    };
    let (vendored, live) = (object(vendored), object(live));
    vendored
        .keys()
        .chain(live.keys())
        .filter(|key| vendored.get(*key) != live.get(*key))
        .cloned()
        .collect()
}

/// What differs, most specific first: the paths (`path /v1/systemone`) and
/// component schemas (`schema ScoreAnswer`) that changed, or, when none
/// did, the other top-level parts (`info`, `openapi`,
/// `components.securitySchemes`).
fn differences(vendored: &Value, live: &Value) -> Vec<String> {
    let mut changed: Vec<String> = differing_keys(vendored, live, "/paths")
        .into_iter()
        .map(|path| format!("path {path}"))
        .chain(
            differing_keys(vendored, live, "/components/schemas")
                .into_iter()
                .map(|schema| format!("schema {schema}")),
        )
        .collect();
    if changed.is_empty() {
        changed = differing_keys(vendored, live, "")
            .into_iter()
            .filter(|key| key != "components" && key != "paths")
            .collect();
        changed.extend(
            differing_keys(vendored, live, "/components")
                .into_iter()
                .filter(|key| key != "schemas")
                .map(|key| format!("components.{key}")),
        );
    }
    changed
}

fn version(document: &Value) -> &str {
    document
        .pointer("/info/version")
        .and_then(Value::as_str)
        .unwrap_or("(none)")
}

#[tokio::test]
#[ignore = "fetches https://api.typesafe.ai/openapi.json: needs the network, no key"]
async fn the_vendored_openapi_document_is_the_live_one() {
    let response = reqwest::get(LIVE_URL)
        .await
        .and_then(reqwest::Response::error_for_status)
        .unwrap_or_else(|e| panic!("cannot fetch {LIVE_URL}: {e}"));
    let text = response
        .text()
        .await
        .unwrap_or_else(|e| panic!("cannot read {LIVE_URL}: {e}"));
    let live: Value =
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("{LIVE_URL} is not JSON: {e}"));

    let path = fixture_path();
    let vendored: Value = match std::fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str(&text)
            .unwrap_or_else(|e| panic!("{} is not JSON: {e}", path.display())),
        // Missing: the first run with the write variable creates it.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Value::Null,
        Err(e) => panic!("cannot read {}: {e}", path.display()),
    };

    if std::env::var_os(WRITE_VAR).is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, canonical(&live)).unwrap();
        eprintln!(
            "wrote {} from {LIVE_URL} (info.version {} -> {}); now run \
             `cargo test -p judgment --test contract` and review the diff",
            path.display(),
            version(&vendored),
            version(&live),
        );
        return;
    }

    if vendored != live {
        let changed = differences(&vendored, &live);
        panic!(
            "the vendored OpenAPI document is not the live one\n\
             vendored info.version: {}\n\
             live info.version:     {}\n\
             differs: {}\n\
             refresh it with\n  \
             {WRITE_VAR}=1 cargo test -p judgment --test openapi_drift -- --ignored\n\
             then rerun `cargo test -p judgment --test contract`: a gap it pins \
             that the new document closes fails at its own path, and every \
             request, Fake and recording is checked against the new schemas",
            version(&vendored),
            version(&live),
            if changed.is_empty() {
                "(nothing named; compare the files)".to_owned()
            } else {
                changed.join(", ")
            },
        );
    }
}
