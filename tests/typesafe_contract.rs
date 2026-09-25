//! signalman's own TypeSafe traffic against the published contract: every
//! triage request it builds, the TypeSafe mocks the other test files share,
//! and the committed Jev run, each checked against the OpenAPI document the
//! judgment crate vendors (`crates/judgment/tests/fixtures/typesafe-openapi.json`,
//! refreshed only through `crates/judgment/tests/openapi_drift.rs`).
//!
//! The judgment crate's `tests/contract.rs` covers what its builders can
//! produce in general. This file covers what signalman actually sends: the
//! triage questions carry structured instructions (`question`, `guidance`,
//! `catalog`, `context`) and runtime option sets, and the state is a whole
//! alert, so a change to either that the schema would refuse, or that names
//! a request field the schema does not, fails here. It also keeps the shared
//! mocks honest: a fixture response the schema refuses would make every test
//! that uses it test a server that does not exist.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::LazyLock;

use serde_json::{Value, json};
use signalman::Request;
use signalman::client::DEFAULT_MODEL;
use signalman::eval::Recording;
use signalman::triage::{
    Alert, ComponentContext, OpenIncident, OwnerCandidate, OwnerCandidates, RelatedAlert, Texts,
    TriageQuestions,
};

static SPEC: LazyLock<Value> = LazyLock::new(|| {
    serde_json::from_str(include_str!(
        "../crates/judgment/tests/fixtures/typesafe-openapi.json"
    ))
    .expect("the vendored OpenAPI document is JSON")
});

/// `POST /v1/systemone`'s request body, its 200, and `GET /v1/models`'s 200.
const REQUEST_BODY: &str =
    "/paths/~1v1~1systemone/post/requestBody/content/application~1json/schema";
const RESPONSE_200: &str =
    "/paths/~1v1~1systemone/post/responses/200/content/application~1json/schema";
const MODELS_200: &str = "/paths/~1v1~1models/get/responses/200/content/application~1json/schema";

/// The component the operation's `$ref` at `pointer` names.
fn schema_of(pointer: &str) -> String {
    let reference = SPEC
        .pointer(pointer)
        .and_then(|schema| schema.get("$ref"))
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("no $ref at {pointer} in the vendored document"));
    reference
        .strip_prefix("#/components/schemas/")
        .unwrap_or_else(|| panic!("{pointer} refers outside the components: {reference}"))
        .to_owned()
}

/// The components with the request's own objects closed:
/// `unevaluatedProperties: false` on the request, the three question kinds
/// and the Noul criteria, so a key the document does not name fails. None of
/// them is closed in the document, and most request fields are optional, so
/// against the open components a renamed or misspelt optional field is only
/// an extra key and conforms. The keyword does not reach into a property's
/// value, so `state`, the `questions` map and the Choice and Score criteria
/// stay open. `crates/judgment/tests/contract.rs` derives the set from the
/// document and fails when it is no longer these five.
static STRICT_COMPONENTS: LazyLock<Value> = LazyLock::new(|| {
    let mut components = SPEC["components"].clone();
    for name in [
        "SystemOneRequest",
        "NoulQuestion",
        "ChoiceQuestion",
        "ScoreQuestion",
        "NoulCriteria",
    ] {
        components["schemas"][name]
            .as_object_mut()
            .unwrap_or_else(|| panic!("the vendored document has no component {name}"))
            .insert("unevaluatedProperties".to_owned(), Value::Bool(false));
    }
    components
});

/// Every failure under `error`, the `anyOf` and `oneOf` branches included,
/// as `path: message [schema path]`.
fn flatten(error: &jsonschema::ValidationError<'_>, out: &mut Vec<String>) {
    use jsonschema::error::ValidationErrorKind as Kind;
    let mut message = error.to_string();
    // A failing `oneOf` quotes the whole instance. Cut at 200 characters,
    // not bytes, or non-ASCII text in it panics here and loses the list.
    if let Some((cut, _)) = message.char_indices().nth(200) {
        message.truncate(cut);
    }
    out.push(format!(
        "{}: {message} [schema {}]",
        error.instance_path(),
        error.schema_path()
    ));
    if let Kind::AnyOf { context }
    | Kind::OneOfNotValid { context }
    | Kind::OneOfMultipleValid { context } = error.kind()
    {
        for nested in context.iter().flatten() {
            flatten(nested, out);
        }
    }
}

/// Every failure of `instance` against the component the operation at
/// `pointer` names, validated as JSON Schema 2020-12 (OAS 3.1) over
/// `components`: the document's, or [`STRICT_COMPONENTS`].
fn failures(pointer: &str, components: &Value, instance: &Value) -> Vec<String> {
    let schema = schema_of(pointer);
    let validator = jsonschema::draft202012::new(&json!({
        "$ref": format!("#/components/schemas/{schema}"),
        "components": components,
    }))
    .unwrap_or_else(|e| panic!("component {schema} does not compile: {e}"));
    let mut failures = Vec::new();
    for error in validator.iter_errors(instance) {
        flatten(&error, &mut failures);
    }
    failures
}

/// `instance` is valid against the component the operation at `pointer`
/// names.
#[track_caller]
fn assert_conforms(pointer: &str, instance: &Value, what: &str) {
    let failures = failures(pointer, &SPEC["components"], instance);
    assert!(
        failures.is_empty(),
        "{what} is not a valid {}:\n  {}\ninstance: {instance:#}",
        schema_of(pointer),
        failures.join("\n  ")
    );
}

/// `instance` is valid against the component the operation at `pointer`
/// names with the request components closed: it conforms, and it names no
/// field the document does not.
#[track_caller]
fn assert_conforms_strict(pointer: &str, instance: &Value, what: &str) {
    let failures = failures(pointer, &STRICT_COMPONENTS, instance);
    assert!(
        failures.is_empty(),
        "{what} is not a valid {} with the request components closed (a field \
         the document does not name?):\n  {}\ninstance: {instance:#}",
        schema_of(pointer),
        failures.join("\n  ")
    );
}

/// The request `signalman triage` sends for `alert`, built as `src/main.rs`
/// builds it.
fn triage_request(alert: &Alert, candidates: OwnerCandidates) -> Value {
    let questions =
        TriageQuestions::for_alert_with_texts(alert, candidates, &Texts::default()).unwrap();
    let state = TriageQuestions::state(alert);
    serde_json::to_value(Request {
        state: &state,
        model: DEFAULT_MODEL,
        questions: &questions.questions,
    })
    .unwrap()
}

/// An alert with every optional part set, so every conditional question and
/// instruction is asked: catalog guidance on the owner, blast-radius context
/// on the impact, the dedup Choice and the change Noul.
fn every_part_alert() -> Alert {
    Alert {
        source: "prometheus".into(),
        title: "HighErrorRate checkout-api".into(),
        description: "5xx ratio 12% for 10m on checkout-api".into(),
        labels: BTreeMap::from([
            ("service".into(), "checkout-api".into()),
            ("severity".into(), "critical".into()),
        ]),
        runbook: Some("Check payments-gateway health before rolling back.".into()),
        recent_changes: vec!["2026-09-20T11:50Z Argo CD sync checkout-api to v2.14.0".into()],
        open_incidents: vec![OpenIncident {
            id: "INC-4821".into(),
            summary: "Checkout 5xx spike".into(),
        }],
        component: Some(ComponentContext {
            name: "checkout-api".into(),
            title: Some("Checkout API".into()),
            component_type: Some("service".into()),
            lifecycle: Some("production".into()),
            system: Some("commerce".into()),
            owner: Some("Payments".into()),
            owner_description: Some("Checkout and payment processing".into()),
            depends_on: vec!["component payments-gateway".into()],
            dependents: vec!["component web-storefront".into()],
            tags: vec!["tier-1".into()],
            links: vec!["Checkout dashboard".into()],
            ..ComponentContext::default()
        }),
        related_alerts: vec![RelatedAlert {
            title: "HighLatency payments-gateway".into(),
            age_minutes: 3,
            component: Some("payments-gateway".into()),
        }],
    }
}

/// Owner candidates as the catalog enrichment returns them: groups with
/// entity refs, before the no-match option is appended.
fn catalog_candidates() -> OwnerCandidates {
    OwnerCandidates::new(vec![
        OwnerCandidate {
            key: "payments".into(),
            label: "Payments".into(),
            description: "Owns checkout-api and payments-gateway".into(),
            entity_ref: Some("group:default/payments".into()),
        },
        OwnerCandidate {
            key: "storefront".into(),
            label: "Storefront".into(),
            description: "Owns web-storefront".into(),
            entity_ref: Some("group:default/storefront".into()),
        },
    ])
}

#[test]
fn the_triage_request_for_every_example_alert_is_a_system_one_request() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/alerts");
    let mut count = 0;
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|e| e != "json") {
            continue;
        }
        let alert: Alert = serde_json::from_str(&std::fs::read_to_string(&path).unwrap())
            .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let request = triage_request(&alert, OwnerCandidates::from_teams());
        assert_conforms_strict(
            REQUEST_BODY,
            &request,
            &format!("the triage request for {}", path.display()),
        );
        count += 1;
    }
    assert!(count > 0, "no example alerts under {}", dir.display());

    let request = triage_request(&every_part_alert(), catalog_candidates());
    let questions = &request["questions"];
    // Not vacuous: every conditional part of the request is present.
    assert!(questions["owner"]["instructions"]["catalog"].is_string());
    assert!(questions["impact"]["instructions"]["context"].is_string());
    assert!(questions["duplicate_of"]["criteria"]["INC-4821"].is_string());
    assert!(questions["caused_by_change"].is_object());
    assert!(questions["owner"]["criteria"]["payments"].is_string());
    assert_conforms_strict(
        REQUEST_BODY,
        &request,
        "the triage request for an alert with every part set",
    );

    // Not vacuous: the owner question with `instructions` misspelt is a
    // valid request against the document as published, and the closed
    // components refuse it at the question.
    let mut misspelt = request.clone();
    let owner = misspelt["questions"]["owner"].as_object_mut().unwrap();
    let instructions = owner.remove("instructions").unwrap();
    owner.insert("instruction".to_owned(), instructions);
    assert_conforms(REQUEST_BODY, &misspelt, "`instruction` for `instructions`");
    let refused = failures(REQUEST_BODY, &STRICT_COMPONENTS, &misspelt);
    assert!(
        refused
            .iter()
            .any(|f| f.starts_with("/questions/owner: ") && f.ends_with("/unevaluatedProperties]")),
        "the closed request components accept `instruction` for `instructions`:\n  {}",
        refused.join("\n  ")
    );
}

#[test]
fn a_failure_message_is_cut_on_a_character_boundary() {
    // A failing `oneOf` quotes the whole question, and each message in the
    // list is cut to 200 characters. Instructions of two-byte characters
    // with and without a one-byte pad put byte 200 inside a character in one
    // of the two, so a cut by bytes would panic in the helper and lose the
    // list; by characters, the quoting message is cut to exactly 200.
    for pad in ["", "x"] {
        let instance = json!({
            "state": "text",
            "model": DEFAULT_MODEL,
            "questions": {
                "q": {
                    "type": "noul",
                    "instructions": format!("{pad}{}", "\u{e9}".repeat(300)),
                    "criteria": { "true": true },
                },
            },
        });
        let refused = failures(REQUEST_BODY, &SPEC["components"], &instance);
        let quoting = refused
            .iter()
            .find(|f| f.starts_with("/questions/q: "))
            .unwrap_or_else(|| panic!("pad {pad:?}: not refused at the question: {refused:?}"));
        let message = quoting
            .strip_prefix("/questions/q: ")
            .and_then(|f| f.rsplit_once(" [schema "))
            .map(|(message, _)| message)
            .unwrap();
        assert!(message.contains('\u{e9}'), "pad {pad:?}: {message}");
        assert_eq!(message.chars().count(), 200, "pad {pad:?}: {message}");
    }
}

#[test]
fn the_shared_typesafe_mocks_are_system_one_responses() {
    for (what, body) in [
        (
            "common::SystemOne::default()",
            common::SystemOne::default().body(),
        ),
        (
            "common::SystemOne::default().attaching()",
            common::SystemOne::default().attaching().body(),
        ),
    ] {
        assert_conforms(RESPONSE_200, &body, what);
    }
    assert_conforms(MODELS_200, &common::models_body(), "common::models_body()");
}

#[test]
fn the_committed_jev_run_is_a_system_one_response() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/eval/runs/jev-1.13.0");
    let mut count = 0;
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|e| e != "json") {
            continue;
        }
        let what = path.display().to_string();
        let text = std::fs::read_to_string(&path).unwrap();
        let raw: Value = serde_json::from_str(&text).unwrap();
        assert_conforms(
            RESPONSE_200,
            &raw["response"],
            &format!("{what} as committed"),
        );
        let recording: Recording = serde_json::from_str(&text).unwrap();
        assert_conforms(
            RESPONSE_200,
            &serde_json::to_value(&recording.response).unwrap(),
            &format!("{what} decoded and re-serialised"),
        );
        count += 1;
    }
    assert!(count > 0, "no recordings under {}", dir.display());
}
