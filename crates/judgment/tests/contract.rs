//! Contract tests: what the crate sends, what its Fake answers and what its
//! committed recordings hold, checked against the OpenAPI document TypeSafe
//! publishes for the System One API.
//!
//! The wiremock tests in `client.rs` encode what this crate's author read in
//! the TypeSafe documentation. This file checks the same wire against the
//! machine-readable contract instead, so a request shape the builders can
//! produce, or a response shape a test fixture assumes, that the published
//! schema does not allow fails here rather than in production.
//!
//! # The fixture
//!
//! `tests/fixtures/typesafe-openapi.json` is <https://api.typesafe.ai/openapi.json>
//! (`openapi` 3.1.0, `info.version` 0.2.0), fetched on 2026-09-25 by
//! `tests/openapi_drift.rs` with `JUDGMENT_OPENAPI_WRITE=1`. It is never
//! edited by hand: the drift test is the only writer, it writes the document
//! canonically (sorted keys, two-space indent, final newline), and
//! `the_schema_has_the_kinds_and_paths_the_crate_has` checks the bytes are
//! still canonical. Vendoring keeps this run offline; the drift test, run by
//! hand, says when the copy is stale.
//!
//! OAS 3.1 schemas are JSON Schema 2020-12, so a component is validated by a
//! 2020-12 validator over `{"$ref": "#/components/schemas/<name>",
//! "components": ..}`. `discriminator` is an annotation and does not affect
//! validation (OAS 3.1.1 §4.8.25); a `oneOf` over the three kinds is what
//! refuses an answer that is none of them. The request, response and
//! model-list schema names are read from the operations, not written here.
//!
//! No component in the document closes its properties, and most request
//! fields are optional, so against the document as published a renamed or
//! misspelt optional field (`instruction`, `critera`) is only an extra key
//! and conforms. Request bodies the builders produce are therefore also
//! checked against a closed overlay: a copy of the components in which the
//! request, each member of its question union and the components those
//! members refer to (`NoulCriteria`) carry `unevaluatedProperties: false`,
//! so a key the document does not name fails. The overlay is built in
//! memory; the fixture is not touched, and `state`, the `questions` map and
//! the Choice and Score criteria stay as open as the document has them.
//!
//! The document's examples live on properties only, so the tests read them
//! by named JSON pointer and assemble instances, asserting each assembled
//! instance conforms before using it: an example a refresh removes fails
//! loudly, not vacuously.
//!
//! # The pinned gaps
//!
//! Where the crate and the schema disagree, a test pins the disagreement in
//! both directions, each case at its own instance path, so a schema refresh
//! that closes a gap fails the case that pinned it:
//!
//! * `what_the_crate_sends_and_the_schema_refuses`: the builders accept any
//!   JSON for state, instructions, criteria and levels, so a null or numeric
//!   state, numeric instructions, a boolean Noul criterion and a numeric
//!   Score level go out although the schema refuses them, as does a request
//!   with no questions.
//! * `what_the_builder_refuses_and_the_schema_allows`: the builder follows
//!   the HTTP API reference page (at most 255 options, 2 to 10 levels); the
//!   schema says only `minItems: 1` for levels and nothing about options.
//! * `what_the_crate_decodes_differently_from_the_schema`: the schema
//!   types probabilities as bare numbers and counts as bare integers, so the
//!   crate refuses values it allows; and the crate tolerates what the schema
//!   requires: `usage` or its counts, including as null, a non-empty
//!   `answers`, a legend entry that is null or a scalar.
//!
//! An answer of a kind the document does not name is also decoded
//! (`Answer::Unknown`) and refused by the schema's `oneOf`. It is not fed to
//! the validator: `the_schema_has_the_kinds_and_paths_the_crate_has` compares
//! the discriminator mappings with the crate's kinds instead, so a document
//! that adds a kind fails there.
//!
//! Three tests drive the client over wiremock and need the `http` feature;
//! the other eight build and run under `--no-default-features`, which is why
//! request bodies there are assembled with `json!` rather than through
//! `judgment::Request` (defined in the client module).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::too_many_lines)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::LazyLock;

use judgment::eval::Recording;
use judgment::{
    Answer, Error, Fake, NoulCriteria, Question, Questions, Response, SystemOne, Usage, options,
};
use serde_json::{Map, Value, json};

#[cfg(feature = "http")]
use judgment::{CallOptions, Client, Request, RetryPolicy};
#[cfg(feature = "http")]
use wiremock::matchers::{method, path};
#[cfg(feature = "http")]
use wiremock::{Mock, MockServer, ResponseTemplate};

// ---------------------------------------------------------------------------
// The document and the validator
// ---------------------------------------------------------------------------

const FIXTURE: &str = include_str!("fixtures/typesafe-openapi.json");

static SPEC: LazyLock<Value> = LazyLock::new(|| {
    serde_json::from_str(FIXTURE).expect("tests/fixtures/typesafe-openapi.json is JSON")
});

/// `POST /v1/systemone`'s request body schema.
const REQUEST_BODY: &str =
    "/paths/~1v1~1systemone/post/requestBody/content/application~1json/schema";
/// `POST /v1/systemone`'s 200 schema.
const RESPONSE_200: &str =
    "/paths/~1v1~1systemone/post/responses/200/content/application~1json/schema";
/// `POST /v1/systemone`'s 422 schema.
#[cfg(feature = "http")]
const RESPONSE_422: &str =
    "/paths/~1v1~1systemone/post/responses/422/content/application~1json/schema";
/// `GET /v1/models`'s 200 schema.
#[cfg(feature = "http")]
const MODELS_200: &str = "/paths/~1v1~1models/get/responses/200/content/application~1json/schema";

/// The document's components with the request's own objects closed (the
/// module docs say why): `unevaluatedProperties: false` on the request
/// schema, on each member of its question union and on each component a
/// member's property refers to, directly or through an `anyOf` or `oneOf`
/// (`NoulCriteria`, reached through `criteria: anyOf [NoulCriteria, null]`).
/// The keyword sits beside each component's own `properties` and does not
/// reach into a property's value, so `state`, the `questions` map and the
/// Choice and Score criteria maps stay open.
static STRICT_COMPONENTS: LazyLock<Value> = LazyLock::new(|| {
    let schemas = &SPEC["components"]["schemas"];
    let component = |reference: &Value| {
        reference
            .as_str()
            .and_then(|r| r.strip_prefix("#/components/schemas/"))
            .map(str::to_owned)
    };
    let union = question_union();
    let members: Vec<String> = schemas[union.as_str()]["oneOf"]
        .as_array()
        .unwrap_or_else(|| panic!("{union} is not a oneOf"))
        .iter()
        .map(|member| {
            component(&member["$ref"])
                .unwrap_or_else(|| panic!("a {union} member is not a component: {member}"))
        })
        .collect();
    let mut closed = BTreeSet::from([request_schema()]);
    for member in &members {
        closed.insert(member.clone());
        let properties = schemas[member.as_str()]["properties"]
            .as_object()
            .unwrap_or_else(|| panic!("{member} has no properties"));
        for property in properties.values() {
            let alternatives = ["anyOf", "oneOf"]
                .iter()
                .filter_map(|k| property[*k].as_array())
                .flatten();
            for schema in std::iter::once(property).chain(alternatives) {
                if let Some(name) = component(&schema["$ref"]) {
                    closed.insert(name);
                }
            }
        }
    }
    // Named, so a document that changes the set is reviewed with the
    // refresh rather than closed silently.
    assert_eq!(
        closed.iter().map(String::as_str).collect::<Vec<_>>(),
        [
            "ChoiceQuestion",
            "NoulCriteria",
            "NoulQuestion",
            "ScoreQuestion",
            "SystemOneRequest"
        ],
        "the request's components are not the ones the closed overlay was \
         reviewed for"
    );
    let mut components = SPEC["components"].clone();
    for name in &closed {
        let schema = components["schemas"][name.as_str()]
            .as_object_mut()
            .unwrap_or_else(|| panic!("no component {name}"));
        assert!(
            schema.contains_key("properties"),
            "{name} has no properties"
        );
        schema.insert("unevaluatedProperties".to_owned(), Value::Bool(false));
    }
    components
});

/// A 2020-12 validator for one component schema, resolving its `$ref`s
/// against `components`.
fn validator_over(components: &Value, schema: &str) -> jsonschema::Validator {
    jsonschema::draft202012::new(&json!({
        "$ref": format!("#/components/schemas/{schema}"),
        "components": components,
    }))
    .unwrap_or_else(|e| panic!("component {schema} does not compile: {e}"))
}

/// A validator for one component schema against the document as published.
fn validator(schema: &str) -> jsonschema::Validator {
    validator_over(&SPEC["components"], schema)
}

/// A validator for one component schema against [`STRICT_COMPONENTS`].
fn strict_validator(schema: &str) -> jsonschema::Validator {
    validator_over(&STRICT_COMPONENTS, schema)
}

/// The component a `$ref` at `pointer` names, such as `SystemOneRequest`
/// for [`REQUEST_BODY`].
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

/// The document's value at `pointer`, typically a property's first example.
#[track_caller]
fn example(pointer: &str) -> Value {
    SPEC.pointer(pointer)
        .cloned()
        .unwrap_or_else(|| panic!("the vendored document has nothing at {pointer}"))
}

/// One validation failure, flattened out of the `anyOf` and `oneOf`
/// branches that contain it.
struct Failure {
    instance_path: String,
    schema_path: String,
    message: String,
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let at = if self.instance_path.is_empty() {
            "(root)"
        } else {
            &self.instance_path
        };
        write!(f, "{at}: {} [schema {}]", self.message, self.schema_path)
    }
}

/// Every failure under `error`, itself first. A `oneOf` over the three
/// question or answer kinds fails at the question as a whole; the branch
/// errors say which field made the kind it was meant to be fail.
fn flatten(error: &jsonschema::ValidationError<'_>, out: &mut Vec<Failure>) {
    use jsonschema::error::ValidationErrorKind as Kind;
    let mut message = error.to_string();
    // A failing `oneOf` quotes the whole instance; the path says which. Cut
    // at 200 characters, not bytes: an instance carrying non-ASCII text
    // would otherwise panic here and lose the list.
    if let Some((cut, _)) = message.char_indices().nth(200) {
        message.truncate(cut);
    }
    out.push(Failure {
        instance_path: error.instance_path().to_string(),
        schema_path: error.schema_path().to_string(),
        message,
    });
    if let Kind::AnyOf { context }
    | Kind::OneOfNotValid { context }
    | Kind::OneOfMultipleValid { context } = error.kind()
    {
        for branch in context {
            for nested in branch {
                flatten(nested, out);
            }
        }
    }
}

fn failures_of(validator: &jsonschema::Validator, instance: &Value) -> Vec<Failure> {
    let mut out = Vec::new();
    for error in validator.iter_errors(instance) {
        flatten(&error, &mut out);
    }
    out
}

fn failures(schema: &str, instance: &Value) -> Vec<Failure> {
    failures_of(&validator(schema), instance)
}

fn strict_failures(schema: &str, instance: &Value) -> Vec<Failure> {
    failures_of(&strict_validator(schema), instance)
}

fn listed(failures: &[Failure]) -> String {
    failures
        .iter()
        .map(|f| format!("  {f}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// `instance` is valid against component `schema`.
#[track_caller]
fn assert_conforms(schema: &str, instance: &Value, what: &str) {
    let failures = failures(schema, instance);
    assert!(
        failures.is_empty(),
        "{what} is not a valid {schema}:\n{}\ninstance: {instance:#}",
        listed(&failures)
    );
}

/// `instance` is valid against component `schema` with the request
/// components closed ([`STRICT_COMPONENTS`]): it conforms, and it names no
/// field the document does not.
#[track_caller]
fn assert_conforms_strict(schema: &str, instance: &Value, what: &str) {
    let failures = strict_failures(schema, instance);
    assert!(
        failures.is_empty(),
        "{what} is not a valid {schema} with the request components closed \
         (a field the document does not name?):\n{}\ninstance: {instance:#}",
        listed(&failures)
    );
}

/// `instance` is refused by component `schema`, and some failure is at or
/// under `instance_path` (a JSON pointer; empty is the root), so the refusal
/// is the one the case is about and not an unrelated mistake in the case.
#[track_caller]
fn assert_refused_at(schema: &str, instance: &Value, instance_path: &str, what: &str) {
    let failures = failures(schema, instance);
    assert!(
        !failures.is_empty(),
        "{what}: the {schema} schema now accepts this. The gap this case pinned \
         is closed by the vendored document; update the crate's behaviour and \
         docs, then move the case to the conforming side.\ninstance: {instance:#}"
    );
    let under = |path: &str| {
        instance_path.is_empty()
            || path == instance_path
            || path.starts_with(&format!("{instance_path}/"))
    };
    assert!(
        failures.iter().any(|f| under(&f.instance_path)),
        "{what}: refused by {schema}, but not at {instance_path}:\n{}",
        listed(&failures)
    );
}

/// The body the client sends, built without the client: `Request` is a
/// plain derived struct in the `http`-only client module, and this is the
/// same JSON (`every_request_the_builders_produce_is_a_system_one_request`
/// checks the client's own bytes).
fn body(state: Value, model: &str, questions: &Questions) -> Value {
    let mut body = Map::new();
    body.insert("state".to_owned(), state);
    body.insert("model".to_owned(), Value::from(model));
    body.insert(
        "questions".to_owned(),
        serde_json::to_value(questions).unwrap(),
    );
    Value::Object(body)
}

fn request_schema() -> String {
    schema_of(REQUEST_BODY)
}

fn response_schema() -> String {
    schema_of(RESPONSE_200)
}

/// The request schema's `n`th example of `property`.
fn request_example(property: &str, n: usize) -> Value {
    example(&format!(
        "/components/schemas/{}/properties/{property}/examples/{n}",
        request_schema()
    ))
}

/// The `oneOf` over the question kinds: what the request's `questions` map
/// holds (`Question`).
fn question_union() -> String {
    schema_of(&format!(
        "/components/schemas/{}/properties/questions/additionalProperties",
        request_schema()
    ))
}

/// The `oneOf` over the answer kinds: what the response's `answers` map
/// holds (`Answer`).
fn answer_union() -> String {
    schema_of(&format!(
        "/components/schemas/{}/properties/answers/additionalProperties",
        response_schema()
    ))
}

// ---------------------------------------------------------------------------
// Instances assembled from the document's examples
// ---------------------------------------------------------------------------

/// The component a discriminator maps `kind` to, in the `oneOf` `union`
/// ([`question_union`] or [`answer_union`]).
fn component_for(union: &str, kind: &str) -> String {
    let pointer = format!("/components/schemas/{union}/discriminator/mapping/{kind}");
    let reference = example(&pointer);
    reference
        .as_str()
        .and_then(|r| r.strip_prefix("#/components/schemas/"))
        .unwrap_or_else(|| panic!("{pointer} is not a component reference: {reference}"))
        .to_owned()
}

/// A `kind` answer from its component's property examples: every required
/// property's first example, and `type` from the discriminator key. Asserted
/// valid before it is returned.
fn example_answer(kind: &str) -> Value {
    let union = answer_union();
    let component = component_for(&union, kind);
    let required = example(&format!("/components/schemas/{component}/required"));
    let mut answer = Map::new();
    answer.insert("type".to_owned(), json!(kind));
    for field in required.as_array().unwrap() {
        let field = field.as_str().unwrap();
        if field != "type" {
            answer.insert(
                field.to_owned(),
                example(&format!(
                    "/components/schemas/{component}/properties/{field}/examples/0"
                )),
            );
        }
    }
    let answer = Value::Object(answer);
    assert_conforms(
        &component,
        &answer,
        &format!("the assembled {kind} example"),
    );
    assert_conforms(&union, &answer, &format!("the assembled {kind} example"));
    answer
}

/// A `SystemOneResponse` from its `answers`, `model` and `usage` property
/// examples. Asserted valid before it is returned.
fn example_response() -> Value {
    let schema = response_schema();
    let property = |name: &str| {
        example(&format!(
            "/components/schemas/{schema}/properties/{name}/examples/0"
        ))
    };
    let response = json!({
        "model": property("model"),
        "answers": property("answers"),
        "usage": property("usage"),
    });
    assert_conforms(&schema, &response, "the assembled response example");
    response
}

// ---------------------------------------------------------------------------
// Every question shape the builders produce
// ---------------------------------------------------------------------------

options! {
    enum Tone {
        Angry = "angry" => "An upset or hostile message",
        Calm = "calm" => None,
        Excited = "excited" => "An enthusiastic or eager message",
    }
}

/// One set holding every shape the builders produce: a Noul without
/// criteria and with string, structured and one-sided criteria, and with
/// null instructions; a typed Choice with an undescribed option, dynamic
/// Choices with and without descriptions and at the 255-option limit;
/// Scores at 2 and 10 levels with string, object and array levels; and a
/// Choice and a Score with null instructions. Instructions come as strings,
/// objects and arrays.
fn every_question_shape() -> Questions {
    let mut q = Questions::new();
    q.noul("noul_plain", "Does `message` convey urgency?", None)
        .unwrap();
    q.noul(
        "noul_string_criteria",
        "Is `message` spam?",
        Some(NoulCriteria::new(
            "Unsolicited advertising",
            "A legitimate conversation",
        )),
    )
    .unwrap();
    q.noul(
        "noul_structured_criteria",
        json!({ "task": "Identify unsolicited advertising in `message`." }),
        Some(NoulCriteria::new(
            json!({ "means": "advertising", "examples": ["Buy now", "Limited offer"] }),
            json!(["A question from a customer", "A reply in a thread"]),
        )),
    )
    .unwrap();
    q.noul(
        "noul_one_sided",
        json!([
            "Does `message` ask for a refund?",
            "A complaint alone is not one."
        ]),
        Some(NoulCriteria {
            yes: Some(json!("An explicit request for money back")),
            no: None,
        }),
    )
    .unwrap();
    q.noul(
        "noul_null_instructions",
        (),
        Some(NoulCriteria::new(
            "`message` is unsolicited advertising",
            "`message` is a legitimate conversation",
        )),
    )
    .unwrap();
    q.choice::<Tone>("choice_enum", "What is the tone of `message`?")
        .unwrap();
    q.choice::<Tone>("choice_null_instructions", Value::Null)
        .unwrap();
    q.dynamic_choice(
        "choice_dynamic",
        json!({ "question": "Which passage answers `query`?", "guidance": "Choose `none` when none does." }),
        [
            ("p1".to_owned(), Some("Refund policy, section 2".to_owned())),
            ("p2".to_owned(), None),
            ("none".to_owned(), Some("No passage answers it".to_owned())),
        ],
    )
    .unwrap();
    q.dynamic_choice(
        "choice_255",
        "Which label fits `message`?",
        (0..255).map(|i| {
            (
                format!("label_{i:03}"),
                (i % 2 == 0).then(|| format!("Label {i}")),
            )
        }),
    )
    .unwrap();
    q.score(
        "score_2",
        "How urgent is `message`?",
        ["Can wait", "Needs attention today"],
    )
    .unwrap();
    q.score(
        "score_10",
        json!({ "rate": "How severe is `alert`?" }),
        (0..10).map(|i| match i % 3 {
            0 => json!(format!("Severity {i}")),
            1 => {
                json!({ "level": i, "means": format!("Severity {i}"), "examples": ["one", "two"] })
            }
            _ => json!([format!("Severity {i}"), "with more detail"]),
        }),
    )
    .unwrap();
    q.score(
        "score_null_instructions",
        None::<&str>,
        ["No impact", "Some users", "Everyone"],
    )
    .unwrap();
    q
}

/// A Fake that fits [`every_question_shape`] (or any set): a Noul at 0.7,
/// a Choice between its first two options, a Score spread evenly over its
/// levels.
fn fitting_fake(questions: &Questions) -> Fake {
    let mut fake = Fake::new().model("jev-1.13.0").usage(Usage {
        input_tokens: 881,
        output_tokens: 122,
    });
    for (id, question) in questions.iter() {
        fake = match question {
            Question::Noul { .. } => fake.noul(id, 0.7).unwrap(),
            Question::Choice { criteria, .. } => {
                let mut keys = criteria.keys();
                let (first, second) = (keys.next().unwrap(), keys.next().unwrap());
                fake.choice(id, [(first.as_str(), 0.6), (second.as_str(), 0.4)], 0.5)
                    .unwrap()
            }
            Question::Score { criteria, .. } => {
                let n = u32::try_from(criteria.len()).unwrap();
                fake.score(id, (0..n).map(|_| 1.0 / f64::from(n)), 0.3)
                    .unwrap()
            }
        };
    }
    fake
}

// ---------------------------------------------------------------------------
// 1-3: the client, over wiremock
// ---------------------------------------------------------------------------

#[cfg(feature = "http")]
#[derive(serde::Serialize)]
struct Ticket {
    subject: &'static str,
    message: &'static str,
    tags: Vec<&'static str>,
}

#[cfg(feature = "http")]
#[tokio::test]
async fn every_request_the_builders_produce_is_a_system_one_request() {
    let server = MockServer::start().await;
    let refusal = json!({
        "detail": example(&format!(
            "/components/schemas/{}/properties/detail/examples/0",
            schema_of(RESPONSE_422)
        )),
    });
    Mock::given(method("POST"))
        .and(path("/v1/systemone"))
        .respond_with(ResponseTemplate::new(422).set_body_json(refusal))
        .mount(&server)
        .await;
    let client = Client::builder()
        .api_key("test-key")
        .base_url(server.uri())
        .model("jev-1.13.0")
        .retry(RetryPolicy::none())
        .build()
        .unwrap();
    let q = every_question_shape();

    let object = json!({ "message": "Please help.", "subject": "Duplicate charge" });
    let array = json!([
        { "role": "customer", "text": "I was charged twice." },
        { "role": "agent", "text": "Looking into it." }
    ]);
    let ticket = Ticket {
        subject: "Duplicate charge",
        message: "I was charged twice. Please help.",
        tags: vec!["billing"],
    };
    let mut results = vec![
        client
            .system_one(&"I was charged twice. Please help.", &q)
            .await,
        client.system_one(&object, &q).await,
        client.system_one(&array, &q).await,
        client.system_one(&ticket, &q).await,
        client
            .evaluate(&Request {
                state: &object,
                model: "jev-latest",
                questions: &q,
            })
            .await,
        SystemOne::answer(&client, &object, "jev-latest", &q).await,
    ];
    let options = CallOptions::new().extra("beam_width", 4).unwrap();
    results.push(
        client
            .evaluate_with(
                &Request {
                    state: &object,
                    model: "jev-1.13.0",
                    questions: &q,
                },
                &options,
            )
            .await,
    );
    for result in &results {
        assert!(
            matches!(result, Err(Error::InvalidRequest { status: 422, .. })),
            "the mock refuses every request: {result:?}"
        );
    }

    let operation_path = "/v1/systemone";
    let operation = &SPEC["paths"][operation_path]["post"];
    assert!(
        operation.is_object(),
        "no POST {operation_path} in the document"
    );
    let content_types: Vec<&String> = operation["requestBody"]["content"]
        .as_object()
        .unwrap()
        .keys()
        .collect();
    assert_eq!(content_types, ["application/json"]);
    assert_eq!(operation["security"], json!([{ "HTTPBearer": [] }]));
    assert_eq!(
        SPEC["components"]["securitySchemes"]["HTTPBearer"],
        json!({ "type": "http", "scheme": "bearer" })
    );

    let received = server.received_requests().await.unwrap();
    assert_eq!(
        received.len(),
        results.len(),
        "one request per call, no retry"
    );
    let schema = request_schema();
    for (i, request) in received.iter().enumerate() {
        assert_eq!(request.method.as_str(), "POST", "request {i}");
        assert_eq!(request.url.path(), operation_path, "request {i}");
        let header = |name: &str| {
            request
                .headers
                .get(name)
                .and_then(|v| v.to_str().ok())
                .unwrap_or_else(|| panic!("request {i} has no {name} header"))
        };
        assert_eq!(header("content-type"), content_types[0], "request {i}");
        // HTTP bearer (RFC 6750): the scheme name is case-insensitive.
        let (auth_scheme, token) = header("authorization").split_once(' ').unwrap();
        assert!(
            auth_scheme.eq_ignore_ascii_case("bearer"),
            "request {i}: {auth_scheme}"
        );
        assert_eq!(token, "test-key", "request {i}");

        let sent: Value = serde_json::from_slice(&request.body).unwrap();
        assert_conforms(&schema, &sent, &format!("request {i}"));
        // Every field it names is one the document names, with the same
        // spelling; `beam_width` (request 6, `CallOptions::extra`) is the
        // caller's own extra field and is checked for below.
        let mut documented = sent.clone();
        documented.as_object_mut().unwrap().remove("beam_width");
        assert_conforms_strict(&schema, &documented, &format!("request {i}"));
        assert_eq!(
            sent["questions"],
            serde_json::to_value(&q).unwrap(),
            "request {i} carries the built questions"
        );
    }
    let bodies: Vec<Value> = received
        .iter()
        .map(|r| serde_json::from_slice(&r.body).unwrap())
        .collect();
    assert_eq!(bodies[0]["state"], "I was charged twice. Please help.");
    assert_eq!(bodies[3]["state"]["tags"], json!(["billing"]));
    assert_eq!(bodies[0]["model"], "jev-1.13.0", "the builder's model");
    assert_eq!(bodies[4]["model"], "jev-latest", "the request's model");
    assert_eq!(bodies[6]["beam_width"], 4, "the extra field is sent");
    // The body `body(..)` builds for the no-http tests is the client's.
    assert_eq!(bodies[1], body(object, "jev-1.13.0", &q));
}

#[cfg(feature = "http")]
#[tokio::test]
async fn the_model_list_example_is_what_list_models_returns() {
    let schema = schema_of(MODELS_200);
    let assembled = json!({
        "models": example(&format!("/components/schemas/{schema}/properties/models/examples/0")),
    });
    let fixture: Value = serde_json::from_str(include_str!("fixtures/models.json")).unwrap();
    for (what, served) in [
        ("the assembled example", assembled),
        ("fixtures/models.json", fixture),
    ] {
        assert_conforms(&schema, &served, what);
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&served))
            .expect(1)
            .mount(&server)
            .await;
        let models = Client::builder()
            .api_key("test-key")
            .base_url(server.uri())
            .retry(RetryPolicy::none())
            .build()
            .unwrap()
            .list_models()
            .await
            .unwrap();
        assert!(!models.is_empty(), "{what}");
        assert_eq!(json!({ "models": models }), served, "{what}");
    }
}

#[cfg(feature = "http")]
#[tokio::test]
async fn the_validation_error_example_reads_as_issues() {
    let schema = schema_of(RESPONSE_422);
    let detail = json!({
        "detail": example(&format!("/components/schemas/{schema}/properties/detail/examples/0")),
    });
    let entry = schema_of(&format!(
        "/components/schemas/{schema}/properties/detail/items"
    ));
    let issue = |field: &str| {
        example(&format!(
            "/components/schemas/{entry}/properties/{field}/examples/0"
        ))
    };
    let at_criteria = json!({
        "detail": [{ "loc": issue("loc"), "msg": issue("msg"), "type": issue("type") }],
    });
    for (served, expected) in [
        (detail, "state"),
        (at_criteria, "questions.urgency.score.criteria"),
    ] {
        assert_conforms(&schema, &served, expected);
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/systemone"))
            .respond_with(ResponseTemplate::new(422).set_body_json(&served))
            .expect(1)
            .mount(&server)
            .await;
        let client = Client::builder()
            .api_key("test-key")
            .base_url(server.uri())
            .retry(RetryPolicy::none())
            .build()
            .unwrap();
        let mut q = Questions::new();
        q.noul("urgent", "Is `message` urgent?", None).unwrap();
        let err = client.system_one(&"help", &q).await.unwrap_err();
        let Error::InvalidRequest { status, issues, .. } = &err else {
            panic!("{expected}: {err:?}");
        };
        assert_eq!(*status, 422);
        assert_eq!(issues.len(), 1, "{expected}: {issues:?}");
        assert_eq!(issues[0].path(), expected);
        assert_eq!(issues[0].msg, "Field required");
        assert_eq!(issues[0].kind, "missing");
    }
}

// ---------------------------------------------------------------------------
// 4-5: where the builder and the request schema disagree
// ---------------------------------------------------------------------------

#[test]
fn what_the_crate_sends_and_the_schema_refuses() {
    let schema = request_schema();
    let mut one = Questions::new();
    one.noul("n", "Is `message` urgent?", None).unwrap();

    // No questions: the builder allows an empty set, the schema needs one.
    assert_refused_at(
        &schema,
        &body(json!("text"), "jev-latest", &Questions::new()),
        "/questions",
        "an empty question set",
    );
    // The state is any `Serialize`; the schema takes a string, object or array.
    assert_refused_at(
        &schema,
        &body(Value::Null, "jev-latest", &one),
        "/state",
        "a null state",
    );
    assert_refused_at(
        &schema,
        &body(json!(42), "jev-latest", &one),
        "/state",
        "a number state",
    );
    // Instructions, criteria and levels are `impl Into<Value>`.
    let mut numeric_instructions = Questions::new();
    numeric_instructions.noul("n", 42, None).unwrap();
    assert_refused_at(
        &schema,
        &body(json!("text"), "jev-latest", &numeric_instructions),
        "/questions/n/instructions",
        "number instructions",
    );
    let mut boolean_criterion = Questions::new();
    boolean_criterion
        .noul(
            "n",
            "Is `message` spam?",
            Some(NoulCriteria::new(true, "a real message")),
        )
        .unwrap();
    assert_refused_at(
        &schema,
        &body(json!("text"), "jev-latest", &boolean_criterion),
        "/questions/n/criteria/true",
        "a boolean Noul criterion",
    );
    let mut numeric_level = Questions::new();
    numeric_level
        .score("s", "How urgent is `message`?", [json!(1), json!("high")])
        .unwrap();
    assert_refused_at(
        &schema,
        &body(json!("text"), "jev-latest", &numeric_level),
        "/questions/s/criteria/0",
        "a number Score level",
    );
}

#[test]
fn what_the_builder_refuses_and_the_schema_allows() {
    let schema = request_schema();
    let hand_written = |question: Value| json!({ "state": "text", "model": "jev-latest", "questions": { "q": question } });
    let options = |n: usize| -> Map<String, Value> {
        (0..n).map(|i| (format!("o{i}"), Value::Null)).collect()
    };
    let levels = |n: usize| -> Vec<Value> { (0..n).map(|i| json!(format!("level {i}"))).collect() };

    for n in [1, 256] {
        let refused = Questions::new().dynamic_choice(
            "q",
            "pick",
            options(n).into_iter().map(|(k, _)| (k, None)),
        );
        assert!(
            matches!(refused, Err(Error::InvalidQuestion { .. })),
            "{n} options: {refused:?}"
        );
        assert_conforms(
            &schema,
            &hand_written(
                json!({ "type": "choice", "instructions": "pick", "criteria": options(n) }),
            ),
            &format!("a Choice of {n} options"),
        );
    }
    for n in [1, 11] {
        let refused = Questions::new().score("q", "rate", levels(n));
        assert!(
            matches!(refused, Err(Error::InvalidQuestion { .. })),
            "{n} levels: {refused:?}"
        );
        assert_conforms(
            &schema,
            &hand_written(
                json!({ "type": "score", "instructions": "rate", "criteria": levels(n) }),
            ),
            &format!("a Score of {n} levels"),
        );
    }
    // Not vacuous: the schema's own lower bound on levels holds.
    assert_refused_at(
        &schema,
        &hand_written(json!({ "type": "score", "instructions": "rate", "criteria": [] })),
        "/questions/q",
        "a Score of 0 levels",
    );
}

// ---------------------------------------------------------------------------
// 6-9: responses: the Fake, the recordings, the examples, the decode gaps
// ---------------------------------------------------------------------------

#[tokio::test]
async fn every_fake_response_is_a_system_one_response() {
    let schema = response_schema();
    let q = every_question_shape();
    let state = json!({ "message": "I was charged twice." });

    let response = fitting_fake(&q)
        .answer(&state, "jev-latest", &q)
        .await
        .unwrap();
    assert_eq!(response.answers.len(), q.len());
    assert_conforms(
        &schema,
        &serde_json::to_value(&response).unwrap(),
        "a fitting Fake over every question shape",
    );

    // The defaults: model `fake`, zero usage.
    let mut one = Questions::new();
    one.noul("urgent", "Is `message` urgent?", None).unwrap();
    let bare = Fake::new().noul("urgent", 0.5).unwrap();
    let response = bare.answer(&state, "jev-latest", &one).await.unwrap();
    assert_conforms(
        &schema,
        &serde_json::to_value(&response).unwrap(),
        "a bare Fake",
    );

    // A Fake asked nothing answers nothing, which the schema refuses:
    // `answers` has `minProperties: 1`. Pinned, so a Fake that starts
    // refusing an empty set, or a schema that allows one, is noticed.
    let empty = Fake::new()
        .answer(&state, "jev-latest", &Questions::new())
        .await
        .unwrap();
    assert!(empty.answers.is_empty());
    assert_refused_at(
        &schema,
        &serde_json::to_value(&empty).unwrap(),
        "/answers",
        "a Fake asked nothing",
    );
}

#[test]
fn every_committed_recording_is_a_system_one_response() {
    let schema = response_schema();
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/typed-decisions/recordings");
    let mut count = 0;
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|e| e != "json") {
            continue;
        }
        let what = path.file_name().unwrap().to_string_lossy().into_owned();
        let text = std::fs::read_to_string(&path).unwrap();
        let raw: Value = serde_json::from_str(&text).unwrap();
        assert_conforms(&schema, &raw["response"], &format!("{what} as committed"));
        let recording: Recording = serde_json::from_str(&text).unwrap();
        assert!(
            recording.response.extra.is_empty(),
            "{what}: {:?}",
            recording.response.extra
        );
        assert_conforms(
            &schema,
            &serde_json::to_value(&recording.response).unwrap(),
            &format!("{what} decoded and re-serialised"),
        );
        count += 1;
    }
    assert!(count > 0, "no recordings under {}", dir.display());
}

#[test]
fn the_schema_examples_decode_through_the_crate() {
    // The question examples, built through the builders: the crate
    // produces them exactly.
    let question = |kind: &str, field: &str| {
        let component = component_for(&question_union(), kind);
        example(&format!(
            "/components/schemas/{component}/properties/{field}/examples/0"
        ))
    };
    let mut q = Questions::new();
    let spam = q
        .noul("spam", question("noul", "instructions"), None)
        .unwrap();
    let criteria = question("choice", "criteria");
    let tone = q
        .dynamic_choice(
            "tone",
            question("choice", "instructions"),
            criteria
                .as_object()
                .unwrap()
                .iter()
                .map(|(k, v)| (k.clone(), v.as_str().map(str::to_owned))),
        )
        .unwrap();
    let levels = question("score", "criteria");
    let urgency = q
        .score(
            "urgency",
            question("score", "instructions"),
            levels.as_array().unwrap().clone(),
        )
        .unwrap();
    assert_eq!(
        serde_json::to_value(&q).unwrap()["tone"]["criteria"],
        criteria
    );
    assert_conforms_strict(
        &request_schema(),
        &body(
            request_example("state", 1),
            request_example("model", 0).as_str().unwrap(),
            &q,
        ),
        "the question examples built by the builders",
    );

    // The answer examples, assembled per kind, decode and fit those
    // questions: the Score example's legend is the ScoreQuestion example's
    // levels, and the Choice example's options are its criteria's.
    let mut assembled = example_response();
    assembled["answers"] = json!({
        "spam": example_answer("noul"),
        "tone": example_answer("choice"),
        "urgency": example_answer("score"),
    });
    assert_conforms(&response_schema(), &assembled, "the three answer examples");
    let response: Response = serde_json::from_value(assembled.clone()).unwrap();
    assert!(response.extra.is_empty(), "{:?}", response.extra);
    assert_eq!(serde_json::to_value(&response).unwrap(), assembled);
    response.verify(&q).unwrap();
    assert!((response.get(&spam).unwrap().yes.value() - 0.98).abs() < 1e-12);
    assert_eq!(response.get(&tone).unwrap().chosen, "angry");
    let score = response.get(&urgency).unwrap();
    assert!((score.value - 1.7).abs() < 1e-12);
    assert_eq!(score.nearest_label(), "Needs attention today");

    // The response example as the document assembles it: `billing`, asked
    // by the request example's question, which the builder reproduces.
    let mut billing = Questions::new();
    let handle = billing
        .noul("billing", "Is this message about billing?", None)
        .unwrap();
    assert_eq!(
        serde_json::to_value(&billing).unwrap(),
        request_example("questions", 0)
    );
    let response: Response = serde_json::from_value(example_response()).unwrap();
    response.verify(&billing).unwrap();
    assert_eq!(response.model, "jev-latest");
    assert_eq!(
        response.usage,
        Usage {
            input_tokens: 120,
            output_tokens: 12
        }
    );
    assert!(response.get(&handle).unwrap().is_yes(0.9));
}

#[test]
fn what_the_crate_decodes_differently_from_the_schema() {
    let schema = response_schema();
    let base = example_response();
    // The response example, with one example answer of each kind, and the
    // value at `pointer` set.
    let with = |pointer: &str, value: Value| {
        let mut instance = base.clone();
        instance["answers"] = json!({
            "n": example_answer("noul"),
            "c": example_answer("choice"),
            "s": example_answer("score"),
        });
        *instance
            .pointer_mut(pointer)
            .unwrap_or_else(|| panic!("nothing at {pointer}")) = value;
        instance
    };

    // The schema accepts these; the crate's decode refuses them, because a
    // probability outside [0, 1] or a negative count is not a number any
    // consumer can use, and it is better an error than a threshold crossed.
    for (what, instance) in [
        ("noul 1.2", with("/answers/n/noul", json!(1.2))),
        (
            "a probability 1.5",
            with("/answers/c/probabilities/angry", json!(1.5)),
        ),
        (
            "a confidence -0.1",
            with("/answers/s/confidence", json!(-0.1)),
        ),
        (
            "a negative usage count",
            with("/usage/input_tokens", json!(-1)),
        ),
    ] {
        assert_conforms(&schema, &instance, what);
        let decoded = serde_json::from_value::<Response>(instance);
        assert!(decoded.is_err(), "{what} decoded: {decoded:?}");
    }

    // The schema refuses these; the crate decodes them, because a server
    // that leaves them out still said something readable (and verify, not
    // decode, is what holds a response to its questions).
    let mut no_answers = base.clone();
    no_answers["answers"] = json!({});
    let mut no_usage = base.clone();
    no_usage.as_object_mut().unwrap().remove("usage");
    let mut decoded = BTreeMap::new();
    for (what, instance, at) in [
        ("answers {}", no_answers, "/answers"),
        (
            "legend {\"0\": null}",
            with("/answers/s/legend/0", Value::Null),
            "/answers/s/legend/0",
        ),
        (
            "legend {\"0\": 1}",
            with("/answers/s/legend/0", json!(1)),
            "/answers/s/legend/0",
        ),
        ("a response without usage", no_usage, ""),
        ("usage null", with("/usage", Value::Null), "/usage"),
        (
            "a null usage count",
            with("/usage/input_tokens", Value::Null),
            "/usage/input_tokens",
        ),
    ] {
        assert_refused_at(&schema, &instance, at, what);
        let response = serde_json::from_value::<Response>(instance)
            .unwrap_or_else(|e| panic!("{what} did not decode: {e}"));
        decoded.insert(what, response);
    }
    // What each tolerated value reads as.
    for what in ["a response without usage", "usage null"] {
        assert_eq!(
            decoded[what].usage,
            Usage::default(),
            "{what} reads as zero"
        );
    }
    let example_usage: Usage = serde_json::from_value(base["usage"].clone()).unwrap();
    assert_eq!(
        decoded["a null usage count"].usage,
        Usage {
            input_tokens: 0,
            ..example_usage
        },
        "a null count reads as zero and the other is kept"
    );
    for (what, level) in [
        ("legend {\"0\": null}", Value::Null),
        ("legend {\"0\": 1}", json!(1)),
    ] {
        let Answer::Score { legend, .. } = &decoded[what].answers["s"] else {
            panic!("{what}: {:?}", decoded[what].answers["s"]);
        };
        assert_eq!(legend["0"], level, "{what} is kept as it came");
    }

    // Both accept these: the response schema is open, so an extra top-level
    // key is valid, and the crate keeps it in `extra`; a body `request_id`
    // decodes into the field the client overwrites from the header.
    let mut open = base.clone();
    open["routing"] = json!({ "checkpoint": "typed-decisions" });
    open["request_id"] = json!("req_body");
    assert_conforms(&schema, &open, "an extra key and a body request_id");
    let decoded: Response = serde_json::from_value(open.clone()).unwrap();
    assert_eq!(decoded.extra.keys().collect::<Vec<_>>(), ["routing"]);
    assert_eq!(decoded.request_id.as_deref(), Some("req_body"));

    // The schema does not define `request_id`, so any type conforms; one
    // that is not a string reads as none rather than failing the response.
    for id in [json!(123), json!({ "id": "x" })] {
        open["request_id"] = id;
        assert_conforms(&schema, &open, "a body request_id that is not a string");
        let decoded: Response = serde_json::from_value(open.clone()).unwrap();
        assert_eq!(decoded.request_id, None, "{}", open["request_id"]);
        assert_eq!(decoded.extra.keys().collect::<Vec<_>>(), ["routing"]);
    }
}

// ---------------------------------------------------------------------------
// 10: the document names what the crate names
// ---------------------------------------------------------------------------

#[test]
fn the_schema_has_the_kinds_and_paths_the_crate_has() {
    // The kinds the builders produce, from the crate itself.
    let mut q = Questions::new();
    q.noul("n", "?", None).unwrap();
    q.choice::<Tone>("c", "?").unwrap();
    q.score("s", "?", ["low", "high"]).unwrap();
    let crate_kinds: BTreeSet<&str> = q.iter().map(|(_, question)| question.kind()).collect();
    for union in [question_union(), answer_union()] {
        let mapped: BTreeSet<&str> = SPEC["components"]["schemas"][union.as_str()]["discriminator"]
            ["mapping"]
            .as_object()
            .unwrap_or_else(|| panic!("{union} has no discriminator mapping"))
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            mapped, crate_kinds,
            "the document's {union} kinds are not the crate's. A new kind \
             decodes as `Answer::Unknown` until the crate learns it: add the \
             `Question` variant and its builder, the `Answer` variant, \
             `Response::verify` and the typed view"
        );
    }
    for kind in &crate_kinds {
        let decoded: Answer = serde_json::from_value(example_answer(kind)).unwrap();
        assert_eq!(decoded.kind(), *kind);
    }

    let paths: BTreeSet<&str> = SPEC["paths"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        paths,
        BTreeSet::from(["/v1/models", "/v1/systemone"]),
        "the document's operations are not the two the client calls"
    );

    // Not vacuous: the request schema refuses a body without its model.
    let mut no_model = body(json!("text"), "jev-latest", &q);
    no_model.as_object_mut().unwrap().remove("model");
    assert_refused_at(&request_schema(), &no_model, "", "a request without model");

    // The field names: requests are also checked with the request
    // components closed, which is what refuses a key the document does not
    // name. Not vacuous: each misspelling below is a valid request against
    // the document as published, and the closed components refuse it at the
    // object that holds it.
    let hand_written = |question: Value| json!({ "state": "text", "model": "jev-latest", "questions": { "q": question } });
    for (what, instance, at) in [
        (
            "`instruction` for `instructions`",
            hand_written(json!({ "type": "noul", "instruction": "Is `message` urgent?" })),
            "/questions/q",
        ),
        (
            "`critera` for `criteria`",
            hand_written(json!({
                "type": "noul",
                "instructions": "Is `message` spam?",
                "critera": { "true": "advertising", "false": "a real message" },
            })),
            "/questions/q",
        ),
        (
            "Noul criteria keyed `yes` and `no`",
            hand_written(json!({
                "type": "noul",
                "instructions": "Is `message` spam?",
                "criteria": { "yes": "advertising", "no": "a real message" },
            })),
            "/questions/q/criteria",
        ),
        (
            "`modle` for `model` beside the model",
            json!({
                "state": "text",
                "model": "jev-latest",
                "modle": "jev-1.13.0",
                "questions": { "q": { "type": "noul", "instructions": "?" } },
            }),
            "",
        ),
    ] {
        assert_conforms(&request_schema(), &instance, what);
        let failures = strict_failures(&request_schema(), &instance);
        assert!(
            failures.iter().any(|f| f.instance_path == at
                && f.schema_path.ends_with("/unevaluatedProperties")),
            "{what}: the closed request components do not refuse it at {}:\n{}",
            if at.is_empty() { "(root)" } else { at },
            listed(&failures)
        );
    }

    // The fixture is canonical: exactly what the drift test writes.
    assert_eq!(
        serde_json::to_string_pretty(&*SPEC).unwrap() + "\n",
        FIXTURE,
        "tests/fixtures/typesafe-openapi.json is not in canonical form (sorted \
         keys, two-space indent, final newline). Rewrite it with \
         `JUDGMENT_OPENAPI_WRITE=1 cargo test -p judgment --test openapi_drift -- --ignored`"
    );
}

// ---------------------------------------------------------------------------
// 11: the failure list survives any text
// ---------------------------------------------------------------------------

#[test]
fn a_failure_message_is_cut_on_a_character_boundary() {
    // A failing `oneOf` quotes the whole question, and each message in the
    // list is cut to 200 characters. Instructions of two-byte characters
    // with and without a one-byte pad put byte 200 inside a character in one
    // of the two, so a cut by bytes would panic in the helper and lose the
    // list; by characters, the message is cut to exactly 200 of them.
    for pad in ["", "x"] {
        let instructions = format!("{pad}{}", "\u{e9}".repeat(300));
        let instance = json!({
            "state": "text",
            "model": "jev-latest",
            "questions": {
                "q": { "type": "noul", "instructions": instructions, "criteria": { "true": true } },
            },
        });
        assert_refused_at(
            &request_schema(),
            &instance,
            "/questions/q/criteria/true",
            "a boolean Noul criterion beside non-ASCII instructions",
        );
        let failures = failures(&request_schema(), &instance);
        let cut: Vec<&Failure> = failures
            .iter()
            .filter(|f| f.message.contains('\u{e9}'))
            .collect();
        assert!(
            cut.iter().any(|f| f.message.chars().count() == 200),
            "pad {pad:?}: no message quoting the instructions was cut:\n{}",
            listed(&failures)
        );
        assert!(
            failures.iter().all(|f| f.message.chars().count() <= 200),
            "pad {pad:?}:\n{}",
            listed(&failures)
        );
    }
}
