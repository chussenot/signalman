//! The triage rubric against the requests signalman sent before it existed.
//!
//! `tests/fixtures/triage-requests.json` was captured from the code that
//! built the questions from `Texts` (signalman on judgment 0.3, 2026-10-04):
//! for alerts with nothing, with everything, with catalog candidates, with
//! the state rule off, and for every committed evaluation case. The built-in
//! rubric must ask exactly those questions, word for word, which is what
//! lets the committed evaluation runs keep replaying: their answers were
//! given to these requests. Order is not compared here: under judgment 0.3
//! the wire sorted questions and options, and from 0.4 it follows the
//! rubric (`options_go_out_in_order_with_the_no_match_option_last`).

#![allow(clippy::unwrap_used, clippy::missing_panics_doc)]

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::Value;
use signalman::eval;
use signalman::triage::{
    Alert, ComponentContext, OpenIncident, OwnerCandidate, OwnerCandidates, RelatedAlert,
    TriageQuestions, TriageRubric,
};

fn bare() -> Alert {
    Alert {
        source: "prometheus".into(),
        title: "KubePodCrashLooping".into(),
        description: "checkout-api restarting".into(),
        labels: BTreeMap::new(),
        runbook: None,
        recent_changes: vec![],
        open_incidents: vec![],
        component: None,
        related_alerts: vec![],
    }
}

fn full() -> Alert {
    let mut a = bare();
    a.component = Some(ComponentContext {
        name: "checkout-api".into(),
        owner: Some("Payments".into()),
        ..Default::default()
    });
    a.related_alerts.push(RelatedAlert {
        title: "HighErrorRate checkout".into(),
        age_minutes: 4,
        component: Some("checkout-api".into()),
    });
    a.open_incidents.push(OpenIncident {
        id: "INC-1".into(),
        summary: "checkout down".into(),
    });
    a.recent_changes.push("deploy checkout-api v2".into());
    a
}

fn catalog() -> OwnerCandidates {
    OwnerCandidates::new(vec![OwnerCandidate {
        key: "payments".into(),
        label: "Payments".into(),
        description: "Owns checkout".into(),
        entity_ref: Some("group:default/payments".into()),
    }])
}

#[test]
fn the_builtin_rubric_asks_exactly_what_the_texts_asked() {
    let golden: BTreeMap<String, Value> = serde_json::from_str(
        &std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/triage-requests.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let builtin = TriageRubric::builtin();
    let no_rule = builtin.without_part("rule").unwrap();
    let mut scenarios: Vec<(String, Alert, OwnerCandidates, &TriageRubric)> = vec![
        (
            "bare".into(),
            bare(),
            OwnerCandidates::from_teams(),
            builtin,
        ),
        (
            "full".into(),
            full(),
            OwnerCandidates::from_teams(),
            builtin,
        ),
        ("full-catalog".into(), full(), catalog(), builtin),
        (
            "full-no-rule".into(),
            full(),
            OwnerCandidates::from_teams(),
            &no_rule,
        ),
    ];
    let cases =
        eval::read_cases(&Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/eval/cases.jsonl"))
            .unwrap();
    for case in cases {
        let id = case.id.clone();
        scenarios.push((
            format!("case-{id}"),
            case.alert.clone(),
            OwnerCandidates::from_teams(),
            builtin,
        ));
        scenarios.push((
            format!("case-{id}-no-rule"),
            case.alert,
            OwnerCandidates::from_teams(),
            &no_rule,
        ));
    }
    assert_eq!(
        scenarios.len(),
        golden.len(),
        "a scenario was added or dropped"
    );
    for (name, alert, candidates, rubric) in scenarios {
        let q = TriageQuestions::for_alert_with_rubric(&alert, candidates, rubric).unwrap();
        let expected = &golden[&name];
        assert_eq!(
            serde_json::to_value(&q.questions).unwrap(),
            expected["questions"],
            "{name}: the questions differ from what the texts asked"
        );
        assert_eq!(
            TriageQuestions::state(&alert),
            expected["state"],
            "{name}: state"
        );
    }
}

#[test]
fn a_rubric_file_named_in_the_configuration_is_what_is_asked() {
    use signalman::config::{Config, Overrides, Settings};

    let dir = std::env::temp_dir().join(format!("signalman-rubric-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let edited = signalman::triage::rubric::BUILTIN.replacen(
        "question: What is the current user-facing impact described by `alert`?",
        "question: How many users does `alert` affect, and how badly?",
        1,
    );
    std::fs::write(dir.join("triage.jud"), edited).unwrap();
    // Relative to the configuration file, not to the working directory.
    let settings = Settings::parse(
        "[triage]\nrubric = \"triage.jud\"\n",
        &dir.join("signalman.toml"),
    )
    .unwrap();
    let cfg = Config::resolve(&settings, &Overrides::default()).unwrap();
    assert_eq!(
        cfg.triage.rubric_path.as_deref(),
        Some(dir.join("triage.jud").as_path())
    );
    let q = TriageQuestions::for_alert_with_rubric(
        &bare(),
        cfg.triage.fallback_candidates(),
        &cfg.triage.rubric,
    )
    .unwrap();
    let json = serde_json::to_value(&q.questions).unwrap();
    assert_eq!(
        json["impact"]["instructions"]["question"],
        "How many users does `alert` affect, and how badly?"
    );
    // The effective configuration names the file and reads back.
    let text = toml::to_string_pretty(&cfg).unwrap();
    assert!(text.contains("rubric = "), "{text}");
    let back = Settings::parse(&text, Path::new("/elsewhere/effective.toml")).unwrap();
    assert_eq!(back.triage.rubric, cfg.triage.rubric_path);

    // A rubric that breaks the contract fails the configuration, naming the file.
    std::fs::write(dir.join("broken.jud"), "jud: 1\nkind: rubric\nid: x\nquestions:\n  owner:\n    type: noul\n    instructions: {question: who?}\n").unwrap();
    let settings = Settings::parse(
        "[triage]\nrubric = \"broken.jud\"\n",
        &dir.join("signalman.toml"),
    )
    .unwrap();
    let err = Config::resolve(&settings, &Overrides::default())
        .unwrap_err()
        .to_string();
    assert!(err.contains("broken.jud"), "{err}");
    assert!(
        err.contains("`owner` is a noul, and signalman reads it as a choice"),
        "{err}"
    );
    let settings = Settings::parse(
        "[triage]\nrubric = \"missing.jud\"\n",
        &dir.join("signalman.toml"),
    )
    .unwrap();
    let err = Config::resolve(&settings, &Overrides::default())
        .unwrap_err()
        .to_string();
    assert!(err.contains("cannot read the rubric"), "{err}");
    std::fs::remove_dir_all(&dir).unwrap();
}
