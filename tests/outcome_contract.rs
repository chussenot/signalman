//! The outcome contract, schema v1.
//!
//! Five things are checked here, because five different things can break it:
//! the committed schema drifting from the types, a field going missing from
//! the wire because it was `None`, a fixture that does not satisfy its own
//! schema, a reader accepting a document it should refuse, and the real
//! binary emitting something else than the library builds.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::too_many_lines)]

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::{Value, json};

mod common;
use common::committed_schema;
use signalman::changes::Change;
use signalman::incidentio::sync::tags_for;
use signalman::outcome::{
    self, AlertRef, Changes, Forwarded, IncidentRef, NoteStatus, NoteWrite, Outcome, WriteMode,
    Writes,
};
use signalman::triage::{
    Alert, ComponentContext, Decision, OpenIncident, OwnerCandidate, OwnerCandidates, Policy,
    RelatedAlert, TriageAnswers, TriageQuestions, decide,
};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Fixed so fixtures are reproducible; the call sites pass the real crate
/// version.
const VERSION: &str = "0.0.0-test";

// ---------------------------------------------------------------------------
// 1. Drift
// ---------------------------------------------------------------------------

#[test]
fn the_committed_schema_matches_the_types() {
    let generated = serde_json::to_value(Outcome::schema()).unwrap();
    let committed = committed_schema();
    assert_eq!(
        generated, committed,
        "docs/schema/outcome.v1.json is out of date.\n\
         Run `mise run schema`, then classify the change:\n\
         - additive (a new always-emitted field, a new nested object, a new \
           value of a free-form string): keep schema_version 1 and update the \
           example in docs/triage.md;\n\
         - breaking (a renamed or removed field, a changed type, unit or \
           nullability, any change to an enum's values): bump to \
           schema_version 2, add docs/schema/outcome.v2.json and keep this \
           file."
    );
}

// ---------------------------------------------------------------------------
// 2. Required is every property, and nothing is ever omitted
// ---------------------------------------------------------------------------

#[test]
fn every_object_requires_every_property() {
    let schema = committed_schema();
    let mut objects = 0;
    check_required(&schema, "#", &mut objects);
    assert!(
        objects >= 15,
        "only {objects} object schemas were checked; the walk is not reaching them"
    );
}

fn check_required(schema: &Value, at: &str, objects: &mut usize) {
    if let Some(map) = schema.as_object() {
        if let Some(props) = map.get("properties").and_then(Value::as_object) {
            *objects += 1;
            let required: Vec<&str> = map
                .get("required")
                .and_then(Value::as_array)
                .map(|r| r.iter().filter_map(Value::as_str).collect())
                .unwrap_or_default();
            let declared: Vec<&str> = props.keys().map(String::as_str).collect();
            assert_eq!(
                required, declared,
                "{at}: `required` must list every property, including the \
                 nullable ones (no field is ever omitted from the wire)"
            );
        }
        for (key, value) in map {
            check_required(value, &format!("{at}/{key}"), objects);
        }
    }
    if let Some(items) = schema.as_array() {
        for (i, value) in items.iter().enumerate() {
            check_required(value, &format!("{at}/{i}"), objects);
        }
    }
}

#[test]
fn every_declared_property_is_present_in_every_fixture() {
    let schema = committed_schema();
    for (name, outcome) in fixtures() {
        let value = serde_json::to_value(&outcome).unwrap();
        assert_present(&schema, &schema, &value, &name);
    }
}

/// Walk schema and document together: every property a schema declares must
/// be a key of the serialised document, `null` or not. Catches a stray
/// `skip_serializing_if`.
fn assert_present(schema: &Value, root: &Value, value: &Value, at: &str) {
    let schema = resolve(schema, root);
    if value.is_null() {
        return;
    }
    if let Some(props) = schema.get("properties").and_then(Value::as_object) {
        let object = value
            .as_object()
            .unwrap_or_else(|| panic!("{at}: expected an object, found {value}"));
        for (key, sub) in props {
            let child = object
                .get(key)
                .unwrap_or_else(|| panic!("{at}.{key} is declared by the schema but not emitted"));
            assert_present(sub, root, child, &format!("{at}.{key}"));
        }
    }
    if let (Some(items), Some(array)) = (schema.get("items"), value.as_array()) {
        for (i, child) in array.iter().enumerate() {
            assert_present(items, root, child, &format!("{at}[{i}]"));
        }
    }
    if let Some(branches) = schema.get("anyOf").and_then(Value::as_array) {
        for branch in branches {
            if resolve(branch, root).get("type").and_then(Value::as_str) == Some("null") {
                continue;
            }
            assert_present(branch, root, value, at);
        }
    }
}

fn resolve<'a>(schema: &'a Value, root: &'a Value) -> &'a Value {
    let Some(reference) = schema.get("$ref").and_then(Value::as_str) else {
        return schema;
    };
    let name = reference.trim_start_matches("#/$defs/");
    root.get("$defs")
        .and_then(|defs| defs.get(name))
        .unwrap_or_else(|| panic!("unresolvable reference {reference}"))
}

// ---------------------------------------------------------------------------
// 3. Fixtures: one per action, both call shapes, plus the write variants
// ---------------------------------------------------------------------------

#[test]
fn every_fixture_validates_round_trips_and_inverts() {
    let schema = committed_schema();
    let validator = jsonschema::validator_for(&schema).expect("the committed schema compiles");

    for (name, outcome) in fixtures() {
        let value = serde_json::to_value(&outcome).unwrap();
        if let Err(e) = validator.validate(&value) {
            panic!("{name} does not satisfy the committed schema: {e}");
        }

        let parsed: Outcome = serde_json::from_value(value).unwrap_or_else(|e| {
            panic!("{name} does not round-trip: {e}");
        });
        assert_eq!(parsed, outcome, "{name} changed across a round trip");

        outcome
            .validate()
            .unwrap_or_else(|e| panic!("{name} is not self-consistent: {e}"));

        assert_eq!(
            outcome.schema_version,
            outcome::SchemaV1,
            "{name} has the wrong version"
        );
    }
}

#[test]
fn the_document_rebuilds_the_decision_it_was_built_from() {
    let mut names = Vec::new();
    for (name, alert, answers, decision, outcome) in decided_fixtures() {
        let _ = alert;
        let _ = answers;
        assert_eq!(
            outcome.decision().unwrap(),
            decision,
            "{name}: the inverse does not return the decision the document was built from"
        );
        // Each fixture decides the action it is named after, on both shapes.
        let (_, action) = name.split_once('/').unwrap();
        assert_eq!(outcome.decision.key(), action, "{name}: {decision:?}");
        names.push(name);
    }
    assert_eq!(names.len(), 10, "{names:?}");
}

#[test]
fn the_tags_are_derivable_from_the_document() {
    for (name, _alert, _answers, _decision, outcome) in decided_fixtures() {
        let expected = outcome.expected_tags();
        for tag in &outcome.tags {
            assert!(
                expected.contains(tag),
                "{name}: tag {tag:?} is not derivable from the document ({expected:?})"
            );
        }
        assert!(
            outcome.tags.iter().any(|t| t.starts_with("ai-action-")),
            "{name}: no action tag"
        );
    }
}

/// Every fixture, keyed by a name that shows up in failure messages.
fn fixtures() -> Vec<(String, Outcome)> {
    let mut all: Vec<(String, Outcome)> = decided_fixtures()
        .into_iter()
        .map(|(name, _, _, _, outcome)| (name, outcome))
        .collect();

    // A note that failed: the tags were still written.
    let (alert, answers, decision) = flow_case(page_answers);
    all.push((
        "flow/page/note-failed".into(),
        flow_outcome(
            &alert,
            &answers,
            &decision,
            Writes {
                mode: WriteMode::Applied,
                tags_applied: true,
                attached: false,
                note: NoteWrite {
                    status: NoteStatus::Failed,
                    id: None,
                    error: Some("incident.io returned 503 Service Unavailable".into()),
                },
                notified: Some("group:default/payments".into()),
                forwarded: None,
            },
        ),
    ));

    // A dry run: everything decided, nothing written.
    let (alert, answers, decision) = flow_case(attach_answers);
    all.push((
        "flow/attach/dry-run".into(),
        flow_outcome(
            &alert,
            &answers,
            &decision,
            Writes {
                mode: WriteMode::DryRun,
                tags_applied: false,
                attached: false,
                note: NoteWrite::skipped(),
                notified: None,
                forwarded: None,
            },
        ),
    ));

    // The CLI with --forward-to-incidentio.
    let (alert, answers, decision) = cli_case(page_answers);
    all.push((
        "cli/page/forwarded".into(),
        cli_outcome(
            &alert,
            &answers,
            &decision,
            Writes {
                forwarded: Some(Forwarded {
                    deduplication_key: "prometheus:KubePodCrashLooping".into(),
                    status: "success".into(),
                }),
                ..Writes::detached()
            },
        ),
    ));

    all
}

/// A fixture's answer set, given who its owner question chooses.
type Answers = fn(&OwnerPick<'_>) -> Value;

/// One fixture per action, on both call shapes, with the alert, the answers
/// and the decision each was built from. Each is named after the action it
/// decides, on both shapes, and
/// `the_document_rebuilds_the_decision_it_was_built_from` holds it to that.
fn decided_fixtures() -> Vec<(String, Alert, TriageAnswers, Decision, Outcome)> {
    let cases: [(&str, Answers); 5] = [
        ("suppress", suppress_answers),
        ("attach_to_incident", attach_answers),
        ("page", page_answers),
        ("ticket", ticket_answers),
        ("human_triage", human_triage_answers),
    ];
    let mut all = Vec::new();
    for (name, answers) in cases {
        let (alert, answers_read, decision) = flow_case(answers);
        let outcome = flow_outcome(&alert, &answers_read, &decision, applied_writes(&decision));
        all.push((
            format!("flow/{name}"),
            alert,
            answers_read,
            decision,
            outcome,
        ));

        let (alert, answers_read, decision) = cli_case(answers);
        let outcome = cli_outcome(&alert, &answers_read, &decision, Writes::detached());
        all.push((
            format!("cli/{name}"),
            alert,
            answers_read,
            decision,
            outcome,
        ));
    }
    all
}

fn applied_writes(decision: &Decision) -> Writes {
    Writes {
        mode: WriteMode::Applied,
        tags_applied: true,
        attached: matches!(decision, Decision::AttachToIncident { .. }),
        note: NoteWrite {
            status: NoteStatus::Created,
            id: Some("note-1".into()),
            error: None,
        },
        notified: decision
            .owner()
            .and_then(|o| o.entity_ref.clone())
            .filter(|_| !matches!(decision, Decision::AttachToIncident { .. })),
        forwarded: None,
    }
}

// ---------------------------------------------------------------------------
// Hand-built answers, read through the real handles
// ---------------------------------------------------------------------------

/// Who a fixture's owner question chooses, among the candidates its call
/// shape offers: a catalog group in the flow, a default team in the CLI. The
/// client refuses an owner the question did not offer, so the answer is
/// built over the case's own candidates.
struct OwnerPick<'a> {
    candidates: &'a OwnerCandidates,
    chosen: &'a str,
}

/// The owner answer: the chosen candidate at `confidence`, every other
/// candidate the question offered at 0.02.
fn owner_answer(pick: &OwnerPick<'_>, confidence: f64) -> Value {
    let probabilities: serde_json::Map<String, Value> = pick
        .candidates
        .iter()
        .map(|c| {
            let p = if c.key == pick.chosen {
                confidence
            } else {
                0.02
            };
            (c.key.clone(), json!(p))
        })
        .collect();
    json!({
        "type": "choice",
        "choice": pick.chosen,
        "probabilities": probabilities,
        "confidence": confidence,
    })
}

fn impact_answer(score: f64) -> Value {
    json!({
        "type": "score",
        "score": score,
        "legend": common::impact_legend(),
        "probabilities": { "0": 0.02, "1": 0.08, "2": 0.8, "3": 0.1 },
        "confidence": 0.86,
    })
}

fn duplicate_answer(chosen: &str, confidence: f64) -> Value {
    json!({
        "type": "choice",
        "choice": chosen,
        "probabilities": { "INC-4821": 0.9, "INC-4900": 0.04, "none": 0.06 },
        "confidence": confidence,
    })
}

fn suppress_answers(owner: &OwnerPick<'_>) -> Value {
    json!({
        "owner": owner_answer(owner, 0.9),
        "impact": impact_answer(0.0),
        "actionable": { "type": "noul", "noul": 0.04 },
        "duplicate_of": duplicate_answer("none", 0.4),
        "caused_by_change": { "type": "noul", "noul": 0.1 },
    })
}

fn attach_answers(owner: &OwnerPick<'_>) -> Value {
    json!({
        "owner": owner_answer(owner, 0.9),
        "impact": impact_answer(2.0),
        "actionable": { "type": "noul", "noul": 0.95 },
        "duplicate_of": duplicate_answer("INC-4821", 0.88),
        "caused_by_change": { "type": "noul", "noul": 0.2 },
    })
}

/// The model names an incident that was never offered: the client refuses
/// it (`an_incident_the_question_never_offered_fails_the_cli_run`) and so
/// does the reader (`an_incident_the_question_never_offered_fails_the_read`),
/// so it is no fixture.
fn hallucinated_dedup_answers(owner: &OwnerPick<'_>) -> Value {
    json!({
        "owner": owner_answer(owner, 0.91),
        "impact": impact_answer(2.0),
        "actionable": { "type": "noul", "noul": 0.95 },
        "duplicate_of": {
            "type": "choice",
            "choice": "INC-9999",
            "probabilities": { "INC-9999": 0.9, "INC-4821": 0.04, "none": 0.06 },
            "confidence": 0.95,
        },
        "caused_by_change": { "type": "noul", "noul": 0.2 },
    })
}

fn page_answers(owner: &OwnerPick<'_>) -> Value {
    json!({
        "owner": owner_answer(owner, 0.91),
        "impact": impact_answer(2.0),
        "actionable": { "type": "noul", "noul": 0.95 },
        "duplicate_of": duplicate_answer("none", 0.5),
        "caused_by_change": { "type": "noul", "noul": 0.87 },
    })
}

fn ticket_answers(owner: &OwnerPick<'_>) -> Value {
    json!({
        "owner": owner_answer(owner, 0.62),
        "impact": impact_answer(1.0),
        "actionable": { "type": "noul", "noul": 0.9 },
        "duplicate_of": duplicate_answer("none", 0.5),
        "caused_by_change": { "type": "noul", "noul": 0.2 },
    })
}

fn human_triage_answers(owner: &OwnerPick<'_>) -> Value {
    json!({
        "owner": owner_answer(owner, 0.2),
        "impact": impact_answer(2.0),
        "actionable": { "type": "noul", "noul": 0.9 },
        "duplicate_of": duplicate_answer("none", 0.5),
        "caused_by_change": { "type": "noul", "noul": 0.2 },
    })
}

/// Read raw answers through the handles the alert produces, exactly as the
/// flow does, so a fixture cannot describe a response the code could not read.
fn read(alert: &Alert, candidates: OwnerCandidates, raw: &Value) -> TriageAnswers {
    let questions = TriageQuestions::for_alert_with(alert, candidates).unwrap();
    let response: signalman::Response = serde_json::from_value(json!({
        "model": "jev-1.13.0",
        "answers": raw.clone(),
        "usage": { "input_tokens": 742, "output_tokens": 41 },
    }))
    .unwrap();
    questions.read(&response).unwrap()
}

// ---------------------------------------------------------------------------
// The two call shapes
// ---------------------------------------------------------------------------

fn catalog_candidates() -> OwnerCandidates {
    OwnerCandidates::new(vec![
        OwnerCandidate {
            key: "payments".into(),
            label: "Payments".into(),
            description: "Checkout and payments".into(),
            entity_ref: Some("group:default/payments".into()),
        },
        OwnerCandidate {
            key: "data-platform".into(),
            label: "Data Platform".into(),
            description: "Owns orders-db".into(),
            entity_ref: Some("group:default/data-platform".into()),
        },
    ])
}

fn component() -> ComponentContext {
    ComponentContext {
        name: "checkout-api".into(),
        title: Some("Checkout API".into()),
        component_type: Some("service".into()),
        lifecycle: Some("production".into()),
        system: Some("shop".into()),
        owner: Some("Payments".into()),
        depends_on: vec!["resource orders-db".into()],
        dependents: vec!["component storefront".into()],
        ..ComponentContext::default()
    }
}

fn labels() -> BTreeMap<String, String> {
    [
        ("Service".to_owned(), "checkout-api".to_owned()),
        ("severity".to_owned(), "critical".to_owned()),
    ]
    .into_iter()
    .collect()
}

fn typed_change() -> Change {
    Change {
        at: Some("2026-09-22T11:42:00Z".into()),
        kind: "deploy".into(),
        component: Some("checkout-api".into()),
        summary: "checkout-api v2.31.0".into(),
        source: Some("argocd".into()),
        url: Some("https://argocd.example.com/applications/checkout-api".into()),
    }
}

/// The webhook flow: an incident.io alert, catalog enrichment, the change
/// feed, and side effects.
fn flow_case(answers: Answers) -> (Alert, TriageAnswers, Decision) {
    let alert = Alert {
        source: "incident.io alert source src-dd".into(),
        title: "HighErrorRate checkout-api".into(),
        description: "5xx ratio 12% for 10m on checkout-api".into(),
        labels: labels(),
        runbook: Some("Roll back the last deploy.".into()),
        recent_changes: vec![typed_change().to_state_line()],
        open_incidents: vec![
            OpenIncident {
                id: "INC-4821".into(),
                summary: "Checkout 5xx spike".into(),
            },
            OpenIncident {
                id: "INC-4900".into(),
                summary: "Search latency".into(),
            },
        ],
        component: Some(component()),
        related_alerts: vec![RelatedAlert {
            title: "HighLatency payments-gateway".into(),
            age_minutes: 4,
            component: Some("payments-gateway".into()),
        }],
    };
    let candidates = catalog_candidates();
    let raw = answers(&OwnerPick {
        candidates: &candidates,
        chosen: "payments",
    });
    let answers = read(&alert, candidates, &raw);
    let decision = decide(&answers, &Policy::default());
    (alert, answers, decision)
}

fn flow_outcome(
    alert: &Alert,
    answers: &TriageAnswers,
    decision: &Decision,
    writes: Writes,
) -> Outcome {
    let candidate_urls: BTreeMap<String, String> = [
        (
            "payments".to_owned(),
            "https://backstage.example.com/catalog/default/group/payments".to_owned(),
        ),
        (
            "data-platform".to_owned(),
            "https://backstage.example.com/catalog/default/group/data-platform".to_owned(),
        ),
    ]
    .into_iter()
    .collect();
    let candidate_incidents = vec![
        IncidentRef {
            id: Some("01INC4821".into()),
            reference: "INC-4821".into(),
            url: Some("https://app.incident.io/org/incidents/4821".into()),
        },
        IncidentRef {
            id: Some("01INC4900".into()),
            reference: "INC-4900".into(),
            url: Some("https://app.incident.io/org/incidents/4900".into()),
        },
    ];
    let incident = match decision {
        Decision::AttachToIncident { incident_id, .. } => candidate_incidents
            .iter()
            .find(|i| &i.reference == incident_id)
            .cloned(),
        _ => None,
    };
    let mut tags = tags_for(answers, decision);
    if let Some(i) = &incident {
        tags.push(format!("ai-dup-{}", i.reference.to_ascii_lowercase()));
    }
    let changes = [typed_change()];

    Outcome::build(outcome::Input {
        version: VERSION,
        decided_at: "2026-09-22T11:58:42Z".parse().unwrap(),
        time_to_qualify: Some(std::time::Duration::from_millis(41_500)),
        alert: AlertRef {
            id: Some("01ALERT4821".into()),
            title: alert.title.clone(),
            source: alert.source.clone(),
            source_url: Some("https://app.datadoghq.eu/monitors/2".into()),
            created_at: Some("2026-09-22T11:58:00Z".into()),
            labels: alert.labels.clone(),
        },
        answers,
        decision,
        policy: &Policy::default(),
        owner_url: decision
            .owner()
            .and_then(|o| candidate_urls.get(&o.key).cloned()),
        candidate_urls: &candidate_urls,
        incident,
        candidate_incidents: &candidate_incidents,
        component: alert.component.as_ref(),
        component_url: Some(
            "https://backstage.example.com/catalog/default/component/checkout-api".into(),
        ),
        runbook_url: Some(
            "https://backstage.example.com/docs/default/component/checkout-api/runbooks/high-error-rate/"
                .into(),
        ),
        related: &alert.related_alerts,
        changes: Changes::Typed(&changes),
        windows: outcome::Windows {
            related_seconds: Some(1800),
            change_seconds: Some(7200),
        },
        tags: &tags,
        model: "jev-1.13.0",
        usage: outcome::Usage {
            input_tokens: 742,
            output_tokens: 41,
        },
        writes,
    })
}

/// The file-based CLI: no alert id, no enrichment, plain change lines, and
/// nothing written back.
fn cli_case(answers: Answers) -> (Alert, TriageAnswers, Decision) {
    let alert = cli_alert();
    let candidates = OwnerCandidates::from_teams();
    let raw = answers(&OwnerPick {
        candidates: &candidates,
        chosen: "application",
    });
    let answers = read(&alert, candidates, &raw);
    let decision = decide(&answers, &Policy::default());
    (alert, answers, decision)
}

/// The alert of the CLI shape: a file, with two open incidents and a change
/// line.
fn cli_alert() -> Alert {
    Alert {
        source: "prometheus".into(),
        title: "KubePodCrashLooping".into(),
        description: "checkout-api restarting, OOMKilled".into(),
        labels: [
            ("service".to_owned(), "checkout-api".to_owned()),
            (
                "source_url".to_owned(),
                "https://prometheus.example.com/alerts?q=KubePodCrashLooping".to_owned(),
            ),
        ]
        .into_iter()
        .collect(),
        runbook: None,
        recent_changes: vec!["2026-09-22T11:42Z deploy checkout-api v2.31.0".into()],
        open_incidents: vec![
            OpenIncident {
                id: "INC-4821".into(),
                summary: "Checkout 5xx spike".into(),
            },
            OpenIncident {
                id: "INC-4900".into(),
                summary: "Search latency".into(),
            },
        ],
        component: None,
        related_alerts: vec![],
    }
}

fn cli_outcome(
    alert: &Alert,
    answers: &TriageAnswers,
    decision: &Decision,
    writes: Writes,
) -> Outcome {
    let candidate_urls = BTreeMap::new();
    let candidate_incidents: Vec<IncidentRef> = alert
        .open_incidents
        .iter()
        .map(|open| IncidentRef {
            id: None,
            reference: open.id.clone(),
            url: None,
        })
        .collect();
    let incident = match decision {
        Decision::AttachToIncident { incident_id, .. } => candidate_incidents
            .iter()
            .find(|i| &i.reference == incident_id)
            .cloned(),
        _ => None,
    };
    let tags = tags_for(answers, decision);

    Outcome::build(outcome::Input {
        version: VERSION,
        decided_at: "2026-09-22T11:58:42Z".parse().unwrap(),
        time_to_qualify: None,
        alert: AlertRef {
            id: None,
            title: alert.title.clone(),
            source: alert.source.clone(),
            source_url: alert.labels.get("source_url").cloned(),
            created_at: None,
            labels: alert.labels.clone(),
        },
        answers,
        decision,
        policy: &Policy::default(),
        owner_url: None,
        candidate_urls: &candidate_urls,
        incident,
        candidate_incidents: &candidate_incidents,
        component: None,
        component_url: None,
        runbook_url: None,
        related: &alert.related_alerts,
        changes: Changes::Lines(&alert.recent_changes),
        windows: outcome::Windows {
            related_seconds: None,
            change_seconds: None,
        },
        tags: &tags,
        model: "jev-1.13.0",
        usage: outcome::Usage {
            input_tokens: 742,
            output_tokens: 41,
        },
        writes,
    })
}

// ---------------------------------------------------------------------------
// 4. What a reader must refuse
// ---------------------------------------------------------------------------

fn a_document() -> Value {
    let (alert, answers, decision) = flow_case(page_answers);
    let outcome = flow_outcome(&alert, &answers, &decision, applied_writes(&decision));
    serde_json::to_value(&outcome).unwrap()
}

#[test]
fn another_schema_version_is_refused_by_name() {
    let mut doc = a_document();
    doc["schema_version"] = json!(2);
    let err = serde_json::from_value::<Outcome>(doc)
        .unwrap_err()
        .to_string();
    assert!(err.contains('2'), "{err}");
    assert!(err.contains("schema_version"), "{err}");
}

#[test]
fn an_unknown_key_is_refused() {
    let mut doc = a_document();
    doc.as_object_mut()
        .unwrap()
        .insert("severity".into(), json!("high"));
    let err = serde_json::from_value::<Outcome>(doc)
        .unwrap_err()
        .to_string();
    assert!(err.contains("severity"), "{err}");
}

#[test]
fn an_unknown_decision_is_refused() {
    let mut doc = a_document();
    doc["decision"] = json!("escalate");
    assert!(serde_json::from_value::<Outcome>(doc).is_err());
}

#[test]
fn a_probability_outside_the_unit_interval_is_refused() {
    let mut doc = a_document();
    doc["judgments"]["actionable"]["probability"] = json!(1.5);
    let err = serde_json::from_value::<Outcome>(doc)
        .unwrap_err()
        .to_string();
    assert!(err.contains("1.5"), "{err}");

    // And the schema says so too, for consumers that never see the Rust type.
    let schema = committed_schema();
    let validator = jsonschema::validator_for(&schema).unwrap();
    let mut doc = a_document();
    doc["judgments"]["owner"]["confidence"] = json!(1.5);
    assert!(validator.validate(&doc).is_err());
}

#[test]
fn a_document_that_contradicts_itself_is_caught_by_validate() {
    let (alert, answers, decision) = flow_case(page_answers);
    let mut outcome = flow_outcome(&alert, &answers, &decision, applied_writes(&decision));
    outcome.impact = outcome::ImpactLevel::None;
    assert!(outcome.validate().is_err());

    let (alert, answers, decision) = flow_case(page_answers);
    let mut outcome = flow_outcome(&alert, &answers, &decision, applied_writes(&decision));
    outcome.owner = None;
    let err = outcome.validate().unwrap_err().to_string();
    assert!(err.contains("owner"), "{err}");
}

/// `signalman triage <file> --json` against a mock TypeSafe that answers
/// `body`: the exit status, stdout and stderr.
async fn run_cli(body: Value) -> std::process::Output {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/systemone"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-typesafe-request-id", "req-cli")
                .set_body_json(body),
        )
        .mount(&server)
        .await;
    let uri = server.uri();
    let alert = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/alerts/crashloop.json");
    // wiremock serves on the test runtime, so the child process runs on a
    // blocking thread rather than on a runtime worker.
    tokio::task::spawn_blocking(move || {
        std::process::Command::new(env!("CARGO_BIN_EXE_signalman"))
            .args(["triage", alert.to_str().unwrap(), "--json"])
            .env("TYPESAFE_BASE_URL", &uri)
            .env("TYPESAFE_API_KEY", "test")
            // Built-in defaults only: no configuration file, no catalog.
            .env_remove("SIGNALMAN_CONFIG")
            .env_remove("BACKSTAGE_BASE_URL")
            .env_remove("BACKSTAGE_NOTIFY")
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .output()
            .expect("the signalman binary runs")
    })
    .await
    .unwrap()
}

#[test]
fn an_incident_the_question_never_offered_fails_the_read() {
    // A response that never went through the client (a recording, a
    // hand-built one) is held to the questions by the reader itself: the
    // unoffered reference is an error naming the question, so it can reach
    // neither the policy nor a document.
    let alert = cli_alert();
    let candidates = OwnerCandidates::from_teams();
    let answers = hallucinated_dedup_answers(&OwnerPick {
        candidates: &candidates,
        chosen: "application",
    });
    let questions = TriageQuestions::for_alert_with(&alert, candidates.clone()).unwrap();
    let response: signalman::Response = serde_json::from_value(json!({
        "model": "jev-1.13.0",
        "answers": answers,
        "usage": { "input_tokens": 742, "output_tokens": 41 },
    }))
    .unwrap();
    let err = questions.read(&response).unwrap_err();
    assert!(
        matches!(
            &err,
            signalman::Error::UnknownOption { id, option, .. }
                if id == "duplicate_of" && option == "INC-9999"
        ),
        "{err:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_incident_the_question_never_offered_fails_the_cli_run() {
    // The model names an incident that was never a candidate. The client
    // refuses the response before signalman reads it, so no document is
    // written and nothing could attach to a reference nobody offered.
    let candidates = OwnerCandidates::from_teams();
    let answers = hallucinated_dedup_answers(&OwnerPick {
        candidates: &candidates,
        chosen: "application",
    });
    let output = run_cli(json!({
        "model": "jev-1.13.0",
        "answers": answers,
        "usage": { "input_tokens": 742, "output_tokens": 41 }
    }))
    .await;
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "the run must fail; stdout: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(stderr.contains("duplicate_of"), "{stderr}");
    assert!(stderr.contains("INC-9999"), "{stderr}");
    assert!(
        stderr.contains("req-cli"),
        "the request id is reported: {stderr}"
    );
    assert!(output.stdout.is_empty(), "no document is written");
}

#[test]
fn a_missing_nullable_key_is_refused() {
    // Every one of these is `required` in the committed schema: a nullable
    // field is present and `null`, never absent. A reader that accepted the
    // key's absence would accept documents the schema rejects.
    let cases: [(&str, &str); 8] = [
        ("", "owner"),
        ("", "incident"),
        ("", "runbook_url"),
        ("", "time_to_qualify_seconds"),
        ("/alert", "source_url"),
        ("/judgments", "duplicate_of"),
        ("/writes", "forwarded"),
        ("/writes/note", "error"),
    ];
    for (parent, key) in cases {
        let mut doc = a_document();
        doc.pointer_mut(parent)
            .unwrap_or_else(|| panic!("{parent} is in the document"))
            .as_object_mut()
            .unwrap()
            .remove(key)
            .unwrap_or_else(|| panic!("{parent}/{key} is in the document"));

        let err = serde_json::from_value::<Outcome>(doc.clone())
            .expect_err(&format!("{parent}/{key} missing must be refused"))
            .to_string();
        assert!(err.contains(key), "{parent}/{key}: {err}");

        // And the schema says the same thing to a consumer with no Rust type.
        let validator = jsonschema::validator_for(&committed_schema()).unwrap();
        assert!(
            validator.validate(&doc).is_err(),
            "{parent}/{key}: the schema accepted a document missing a required key"
        );
    }
}

#[test]
fn only_tags_in_the_signalman_namespace_have_to_be_derivable() {
    let (alert, answers, decision) = flow_case(page_answers);
    let mut outcome = flow_outcome(&alert, &answers, &decision, applied_writes(&decision));

    // Somebody else's tag that happens to start with the same two letters.
    outcome.tags.push("airflow-owned".into());
    outcome.validate().unwrap();

    // One of ours that the document does not imply.
    outcome.tags.push("ai-team-nobody".into());
    let err = outcome.validate().unwrap_err().to_string();
    assert!(err.contains("ai-team-nobody"), "{err}");
}

#[test]
fn an_impact_score_off_the_scale_is_refused() {
    let (alert, answers, decision) = flow_case(page_answers);
    let mut outcome = flow_outcome(&alert, &answers, &decision, applied_writes(&decision));
    outcome.judgments.impact.score = 4.0;
    let err = outcome.validate().unwrap_err().to_string();
    assert!(err.contains("score"), "{err}");

    // The schema bounds it too, along with the length of the distribution.
    let validator = jsonschema::validator_for(&committed_schema()).unwrap();
    let mut doc = a_document();
    doc["judgments"]["impact"]["score"] = json!(99);
    assert!(validator.validate(&doc).is_err());

    let mut doc = a_document();
    doc["judgments"]["impact"]["distribution"]
        .as_array_mut()
        .unwrap()
        .truncate(2);
    assert!(validator.validate(&doc).is_err());
}

// ---------------------------------------------------------------------------
// 5. The real binary
// ---------------------------------------------------------------------------

/// `signalman triage <file> --json` against a mock TypeSafe: the process the
/// docs tell people to run must emit a document that satisfies the committed
/// schema.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_cli_emits_a_document_that_satisfies_the_schema() {
    let output = run_cli(json!({
        "model": "jev-1.13.0",
        "answers": {
            "owner": { "type": "choice", "choice": "application",
                       "probabilities": { "application": 0.82, "platform": 0.1, "database": 0.05, "none_of_these": 0.03 },
                       "confidence": 0.81 },
            "impact": { "type": "score", "score": 2.0,
                        "legend": common::impact_legend(),
                        "probabilities": { "0": 0.02, "1": 0.08, "2": 0.8, "3": 0.1 },
                        "confidence": 0.86 },
            "actionable": { "type": "noul", "noul": 0.94 },
            "duplicate_of": { "type": "choice", "choice": "none",
                              "probabilities": { "INC-4821": 0.18, "none": 0.82 }, "confidence": 0.74 },
            "caused_by_change": { "type": "noul", "noul": 0.88 }
        },
        "usage": { "input_tokens": 742, "output_tokens": 41 }
    }))
    .await;

    assert!(
        output.status.success(),
        "signalman triage failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let document: Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("stdout is not JSON ({e}):\n{stdout}"));

    let schema = committed_schema();
    let validator = jsonschema::validator_for(&schema).unwrap();
    if let Err(e) = validator.validate(&document) {
        panic!("the CLI document does not satisfy the committed schema: {e}\n{stdout}");
    }
    assert_eq!(document["schema_version"], json!(1));
    assert_eq!(document["writes"]["mode"], json!("detached"));
    assert_eq!(document["decision"], json!("page"));
    assert_eq!(document["impact"], json!("major"));
    assert_eq!(document["suspected_change"], json!(true));
    assert_eq!(document["alert"]["id"], Value::Null);
    assert_eq!(document["time_to_qualify_seconds"], Value::Null);

    // The typed reader accepts what the binary wrote, and it holds together.
    let outcome: Outcome = serde_json::from_str(&stdout).unwrap();
    outcome.validate().unwrap();
    assert!(matches!(outcome.decision().unwrap(), Decision::Page { .. }));
}

// ---------------------------------------------------------------------------
// 6. The documented example
// ---------------------------------------------------------------------------

/// The example under `## The outcome contract` in `docs/triage.md` must be a
/// real document: the docs are where consumers learn the shape.
#[test]
fn the_documented_example_validates_against_the_schema() {
    const HEADING: &str = "## The outcome contract";
    let page = Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/triage.md");
    let text = std::fs::read_to_string(&page).expect("docs/triage.md is readable");

    let Some(section) = text.split_once(HEADING).map(|(_, rest)| rest) else {
        // The section is written by the documentation stage. Until it lands
        // there is nothing to check; the moment it does, this test enforces
        // that its example is a document the schema accepts.
        println!(
            "skipped: {} has no `{HEADING}` section yet, so there is no example to validate",
            page.display()
        );
        return;
    };

    let block = section
        .split_once("```json")
        .map(|(_, rest)| rest)
        .and_then(|rest| rest.split_once("```").map(|(block, _)| block))
        .unwrap_or_else(|| {
            panic!(
                "{}: the `{HEADING}` section must contain a ```json example of one outcome \
                 document (produce it with `signalman triage examples/alerts/crashloop.json --json`)",
                page.display()
            )
        });

    let document: Value = serde_json::from_str(block).unwrap_or_else(|e| {
        panic!(
            "{}: the first ```json block under `{HEADING}` is not valid JSON: {e}",
            page.display()
        )
    });

    let schema = committed_schema();
    let validator = jsonschema::validator_for(&schema).unwrap();
    if let Err(e) = validator.validate(&document) {
        panic!(
            "{}: the example under `{HEADING}` does not satisfy docs/schema/outcome.v1.json: {e}",
            page.display()
        );
    }
    let outcome: Outcome = serde_json::from_value(document).unwrap_or_else(|e| {
        panic!(
            "{}: the example under `{HEADING}` cannot be read back as an Outcome: {e}",
            page.display()
        )
    });
    outcome.validate().unwrap_or_else(|e| {
        panic!(
            "{}: the example under `{HEADING}` contradicts itself: {e}",
            page.display()
        )
    });
}
