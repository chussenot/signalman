//! The evaluation harness against a mock TypeSafe: run with recording, grade,
//! then replay the recordings under a different policy without the model. A
//! live run records an answer that does not fit its questions as a failed
//! case and carries on. The committed Jev run decodes, writes back byte for
//! byte, and grades on replay.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use serde_json::json;
use signalman::eval::{self, Action, Setup};
use signalman::triage::{Impact, OwnerCandidates, Policy, Texts};
use signalman::{Client, RetryPolicy};
use wiremock::matchers::{body_partial_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const CASES: &str = r#"
{"id": "oom", "alert": {"source": "prometheus", "title": "KubePodCrashLooping", "description": "checkout-api OOMKilled after deploy", "labels": {"service": "checkout-api"}, "recent_changes": ["deploy checkout-api v2"], "open_incidents": [{"id": "INC-1", "summary": "payments 5xx"}]}, "expected": {"owner": "application", "impact": "major", "actionable": true, "duplicate_of": "none", "caused_by_change": true, "action": "page"}}
{"id": "noise", "alert": {"source": "datadog", "title": "DiskUsageHigh", "description": "staging CI runner at 81%", "labels": {"env": "staging"}}, "expected": {"owner": "platform", "impact": "none", "actionable": false, "duplicate_of": "none", "caused_by_change": false, "action": "suppress"}}
{"id": "unlabelled", "alert": {"source": "x", "title": "Y", "description": "z"}}
"#;

/// The legend TypeSafe echoes for the impact question: its levels as sent,
/// keyed by index. The client checks the echo against the question, so a
/// fixture legend must be the real levels.
fn impact_legend() -> serde_json::Value {
    Impact::LEVELS
        .iter()
        .enumerate()
        .map(|(i, level)| (i.to_string(), json!(level)))
        .collect::<serde_json::Map<String, serde_json::Value>>()
        .into()
}

fn score(level: u8, conf: f64) -> serde_json::Value {
    let mut probs = serde_json::Map::new();
    for i in 0..4u8 {
        probs.insert(i.to_string(), json!(if i == level { 0.85 } else { 0.05 }));
    }
    json!({ "type": "score", "score": f64::from(level), "legend": impact_legend(),
            "probabilities": probs, "confidence": conf })
}

async fn mock_typesafe() -> MockServer {
    let server = MockServer::start().await;
    // oom: right owner with high confidence, wrong dedup (attaches), change yes.
    Mock::given(method("POST"))
        .and(path("/v1/systemone"))
        .and(body_partial_json(json!({ "state": { "alert": { "title": "KubePodCrashLooping" } } })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "model": "jev-1.13.0",
            "answers": {
                "owner": { "type": "choice", "choice": "application",
                           "probabilities": { "application": 0.8, "platform": 0.15, "none_of_these": 0.05 }, "confidence": 0.8 },
                "impact": score(2, 0.9),
                "actionable": { "type": "noul", "noul": 0.95 },
                "duplicate_of": { "type": "choice", "choice": "INC-1",
                                  "probabilities": { "INC-1": 0.9, "none": 0.1 }, "confidence": 0.85 },
                "caused_by_change": { "type": "noul", "noul": 0.8 }
            },
            "usage": { "input_tokens": 500, "output_tokens": 30 }
        })))
        .mount(&server)
        .await;
    // noise: model says actionable 0.6 (wrong), owner database at low confidence (wrong).
    Mock::given(method("POST"))
        .and(path("/v1/systemone"))
        .and(body_partial_json(json!({ "state": { "alert": { "title": "DiskUsageHigh" } } })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "model": "jev-1.13.0",
            "answers": {
                "owner": { "type": "choice", "choice": "database",
                           "probabilities": { "database": 0.4, "platform": 0.35, "none_of_these": 0.25 }, "confidence": 0.3 },
                "impact": score(0, 0.7),
                "actionable": { "type": "noul", "noul": 0.6 }
            },
            "usage": { "input_tokens": 300, "output_tokens": 20 }
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/systemone"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "model": "jev-1.13.0",
            "answers": {
                "owner": { "type": "choice", "choice": "network",
                           "probabilities": { "network": 0.9, "none_of_these": 0.1 }, "confidence": 0.9 },
                "impact": score(1, 0.8),
                "actionable": { "type": "noul", "noul": 0.7 }
            },
            "usage": { "input_tokens": 100, "output_tokens": 10 }
        })))
        .with_priority(9)
        .mount(&server)
        .await;
    server
}

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("signalman-eval-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

#[tokio::test]
async fn run_grades_judgments_and_decisions_and_records_for_replay() {
    let server = mock_typesafe().await;
    let client = Client::builder()
        .api_key("k")
        .base_url(server.uri())
        .retry(RetryPolicy::none())
        .build()
        .unwrap();
    let cases = eval::parse_cases(CASES, "inline").unwrap();
    let texts = Texts::default();
    let candidates = OwnerCandidates::from_teams();
    let policy = Policy::default();
    let setup = Setup {
        texts: &texts,
        candidates: &candidates,
        policy: &policy,
    };
    let dir = tmp("rec");

    let report = eval::run(&client, "jev-latest", &cases, &setup, Some(&dir))
        .await
        .unwrap();
    assert_eq!(report.cases, 3);
    assert_eq!(
        report.models.iter().next().map(String::as_str),
        Some("jev-1.13.0")
    );
    assert_eq!(report.input_tokens, 900);

    // owner: oom right, noise wrong; the unlabelled case is not graded.
    let owner = &report.questions["owner"];
    assert_eq!(owner.labelled, 2);
    assert_eq!(owner.correct, 1);
    assert!((owner.accuracy.unwrap() - 0.5).abs() < 1e-9);
    assert!((owner.confidence_when_right.unwrap() - 0.8).abs() < 1e-9);
    assert!((owner.confidence_when_wrong.unwrap() - 0.3).abs() < 1e-9);
    assert_eq!(owner.confusion["platform"]["database"], 1);

    // impact: both right by nearest level.
    let impact = &report.questions["impact"];
    assert_eq!((impact.labelled, impact.correct), (2, 2));

    // actionable: oom right (0.95), noise wrong (0.6 for a non-actionable alert).
    let act = &report.questions["actionable"];
    assert_eq!((act.labelled, act.correct), (2, 1));

    // duplicate_of: oom wrong (chose INC-1, expected none); noise not asked -> implied none, right.
    let dup = &report.questions["duplicate_of"];
    assert_eq!((dup.labelled, dup.correct), (2, 1));
    let oom = report.graded.iter().find(|g| g.id == "oom").unwrap();
    assert!(
        !report
            .graded
            .iter()
            .find(|g| g.id == "noise")
            .unwrap()
            .judgments["duplicate_of"]
            .asked
    );
    assert!((oom.judgments["duplicate_of"].p_expected.unwrap() - 0.1).abs() < 1e-9);

    // decision: oom attaches (dedup 0.85 >= 0.75) instead of paging; noise is not suppressed.
    assert_eq!(oom.action, Action::AttachToIncident);
    assert_eq!(report.decision.labelled, 2);
    assert_eq!(report.decision.correct, 0);
    assert_eq!(report.decision.confusion["page"]["attach_to_incident"], 1);

    // Brier for a labelled Noul: oom actionable 0.95 vs yes -> (0.05^2 + 0.05^2) = 0.005.
    assert!(report.questions["actionable"].brier.unwrap() > 0.0);
    assert!(report.questions["owner"].ece.is_some());

    // Recordings exist for every case.
    for id in ["oom", "noise", "unlabelled"] {
        assert!(dir.join(format!("{id}.json")).is_file(), "{id}");
    }

    // Replay under a stricter attach threshold flips oom to page, no model call.
    let strict = Policy {
        attach_confidence: 0.9,
        ..Policy::default()
    };
    let setup2 = Setup {
        texts: &texts,
        candidates: &candidates,
        policy: &strict,
    };
    let replayed = eval::replay(&dir, &cases, &setup2).unwrap();
    let oom2 = replayed.graded.iter().find(|g| g.id == "oom").unwrap();
    assert_eq!(oom2.action, Action::Page);
    assert_eq!(replayed.decision.correct, 1);
    assert_eq!(
        replayed.questions["owner"].accuracy,
        report.questions["owner"].accuracy
    );
    assert_eq!(server.received_requests().await.unwrap().len(), 3);

    let text = replayed.render();
    assert!(text.contains("mismatches"), "{text}");
    assert!(
        text.contains("noise") && text.contains("expected platform, got database"),
        "{text}"
    );

    // A missing recording is a named error.
    let cases2 = eval::parse_cases(
        r#"{"id": "ghost", "alert": {"source": "a", "title": "b", "description": "c"}}"#,
        "inline",
    )
    .unwrap();
    assert!(matches!(
        eval::replay(&dir, &cases2, &setup2),
        Err(eval::Error::MissingRecording(id, _)) if id == "ghost"
    ));

    // Impact labels parse from their lowercase names.
    assert_eq!(cases[0].expected.impact, Some(Impact::Major));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_committed_jev_run_decodes_and_rewrites_byte_for_byte() {
    // The raw responses of the first live TypeSafe run (2026-09-23), which
    // `signalman eval --replay` grades offline. judgment's tolerant decoder
    // must read them as the strict one did (nothing unknown, nothing extra)
    // and write them back unchanged.
    let run = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/eval/runs/jev-1.13.0");
    let out = tmp("jev-rewrite");
    let mut seen = 0;
    for entry in std::fs::read_dir(&run).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|e| e != "json") {
            continue;
        }
        let case = path.file_stem().unwrap().to_str().unwrap().to_owned();
        let text = std::fs::read_to_string(&path).unwrap();
        let recording = judgment::eval::read_recording(&run, &case).unwrap();
        assert_eq!(recording.case, case);
        assert_eq!(recording.response.model, "jev-1.13.0");
        assert!(
            recording.response.extra.is_empty(),
            "{case}: {:?}",
            recording.response.extra
        );
        for (id, answer) in &recording.response.answers {
            assert!(
                !matches!(answer, judgment::Answer::Unknown(_)),
                "{case}: {id} is {answer:?}"
            );
        }
        let written = judgment::eval::write_recording(&out, &recording).unwrap();
        assert_eq!(std::fs::read_to_string(&written).unwrap(), text, "{case}");
        seen += 1;
    }
    assert_eq!(seen, 3);
    let _ = std::fs::remove_dir_all(&out);
}

fn default_setup<'a>(
    texts: &'a Texts,
    candidates: &'a OwnerCandidates,
    policy: &'a Policy,
) -> Setup<'a> {
    Setup {
        texts,
        candidates,
        policy,
    }
}

#[test]
fn the_committed_jev_run_grades_on_replay() {
    // The first live TypeSafe run, graded offline against the committed
    // cases: every recorded answer still fits the questions the current
    // default setup asks.
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/eval");
    let cases = eval::read_cases(&root.join("cases.jsonl")).unwrap();
    let (texts, candidates, policy) = (
        Texts::default(),
        OwnerCandidates::from_teams(),
        Policy::default(),
    );
    let report = eval::replay(
        &root.join("runs/jev-1.13.0"),
        &cases,
        &default_setup(&texts, &candidates, &policy),
    )
    .unwrap();
    assert_eq!(report.cases, 3);
    assert!(report.failed.is_empty());
    assert_eq!(
        report.models.iter().map(String::as_str).collect::<Vec<_>>(),
        ["jev-1.13.0"]
    );
    assert_eq!(report.questions["owner"].labelled, 3);
}

#[tokio::test]
async fn a_live_run_records_an_unfit_answer_as_a_failed_case_and_continues() {
    let server = MockServer::start().await;
    // oom: the model names an owner the question never offered.
    Mock::given(method("POST"))
        .and(path("/v1/systemone"))
        .and(body_partial_json(json!({ "state": { "alert": { "title": "KubePodCrashLooping" } } })))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-typesafe-request-id", "req-unfit")
                .set_body_json(json!({
                    "model": "jev-1.13.0",
                    "answers": {
                        "owner": { "type": "choice", "choice": "made-up-team",
                                   "probabilities": { "made-up-team": 0.9, "platform": 0.1 },
                                   "confidence": 0.9 },
                        "impact": score(2, 0.9),
                        "actionable": { "type": "noul", "noul": 0.95 },
                        "duplicate_of": { "type": "choice", "choice": "none",
                                          "probabilities": { "INC-1": 0.1, "none": 0.9 }, "confidence": 0.85 },
                        "caused_by_change": { "type": "noul", "noul": 0.8 }
                    },
                    "usage": { "input_tokens": 500, "output_tokens": 30 }
                })),
        )
        .expect(1)
        .mount(&server)
        .await;
    // noise: answered as asked.
    Mock::given(method("POST"))
        .and(path("/v1/systemone"))
        .and(body_partial_json(json!({ "state": { "alert": { "title": "DiskUsageHigh" } } })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "model": "jev-1.13.0",
            "answers": {
                "owner": { "type": "choice", "choice": "platform",
                           "probabilities": { "platform": 0.8, "none_of_these": 0.2 }, "confidence": 0.8 },
                "impact": score(0, 0.7),
                "actionable": { "type": "noul", "noul": 0.1 }
            },
            "usage": { "input_tokens": 300, "output_tokens": 20 }
        })))
        .expect(1)
        .mount(&server)
        .await;
    let client = Client::builder()
        .api_key("k")
        .base_url(server.uri())
        .retry(RetryPolicy::none())
        .build()
        .unwrap();
    let cases: Vec<_> = eval::parse_cases(CASES, "inline")
        .unwrap()
        .into_iter()
        .filter(|c| c.id != "unlabelled")
        .collect();
    let (texts, candidates, policy) = (
        Texts::default(),
        OwnerCandidates::from_teams(),
        Policy::default(),
    );
    let dir = tmp("unfit");

    let report = eval::run(
        &client,
        "jev-latest",
        &cases,
        &default_setup(&texts, &candidates, &policy),
        Some(&dir),
    )
    .await
    .unwrap();

    assert_eq!(report.failed.len(), 1, "{:?}", report.failed);
    let failed = &report.failed[0];
    assert_eq!(failed.id, "oom");
    assert!(failed.error.contains("\"owner\""), "{}", failed.error);
    assert!(failed.error.contains("made-up-team"), "{}", failed.error);
    assert_eq!(failed.request_id.as_deref(), Some("req-unfit"));

    // The other case is graded, and only it counts.
    assert_eq!(report.cases, 1);
    assert_eq!(report.graded[0].id, "noise");
    assert_eq!(report.input_tokens, 300);
    assert_eq!(report.questions["owner"].labelled, 1);

    // A failed case is not recorded.
    assert!(!dir.join("oom.json").exists());
    assert!(dir.join("noise.json").is_file());

    // The text says which case failed and that the figures leave it out; the
    // JSON lists it.
    let text = report.render();
    assert!(text.contains("failed (1)"), "{text}");
    assert!(text.contains("over the 1 graded cases"), "{text}");
    assert!(text.contains("oom") && text.contains("req-unfit"), "{text}");
    let json = serde_json::to_value(&report).unwrap();
    assert_eq!(json["failed"][0]["id"], "oom");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_report_with_no_failed_case_serialises_as_before() {
    let report = eval::report(Vec::new(), Vec::new());
    let json = serde_json::to_value(&report).unwrap();
    assert!(json.get("failed").is_none(), "{json}");
}
