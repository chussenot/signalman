//! The backends behind one `SystemOne` trait: a `Fake` answers from its
//! table and refuses a question it has no answer for, a `Recorder` writes
//! what another backend answered, and a `Replay` over that directory answers
//! the same request without any backend and refuses a request nobody
//! recorded. Every one of them refuses a response that does not fit its
//! questions: a Fake's script, a recording, what a Recorder's inner backend
//! returned. The client's implementation is checked in `client.rs`. The
//! committed Laya recordings decode, with nothing unknown and nothing extra,
//! write back byte for byte, and replay against the sample they answer.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::io::BufRead;
use std::path::{Path, PathBuf};

use judgment::backend::BoxFuture;
use judgment::eval::{Recording, request_hash, write_recording};
use judgment::{
    Answer, Error, Fake, NoulCriteria, Questions, Recorder, Replay, Response, SystemOne, options,
};
use serde_json::{Value, json};

options! {
    enum Department {
        Billing = "billing" => "Money",
        Technical = "technical" => "Bugs",
    }
}

fn questions() -> (
    Questions,
    judgment::Handle<judgment::Choice<Department>>,
    judgment::Handle<judgment::Noul>,
) {
    let mut q = Questions::new();
    let dept = q
        .choice::<Department>("department", "Which team handles `message`?")
        .unwrap();
    let urgent = q.noul("urgent", "Is `message` urgent?", None).unwrap();
    (q, dept, urgent)
}

fn fake() -> Fake {
    Fake::new()
        .model("fake-1")
        .choice("department", [("billing", 0.9), ("technical", 0.1)], 0.8)
        .unwrap()
        .noul("urgent", 0.2)
        .unwrap()
}

#[tokio::test]
async fn a_fake_answers_typed_and_remembers_the_call() {
    let (q, dept, urgent) = questions();
    let backend = fake();
    let state = json!({ "message": "charged twice" });

    let response = backend.answer(&state, "any-model", &q).await.unwrap();
    assert_eq!(response.model, "fake-1");
    assert_eq!(response.request_id, None, "a fake has no call to name");
    let d = response.get(&dept).unwrap();
    assert_eq!(d.chosen, Department::Billing);
    assert!(d.confidence.at_least(0.8));
    assert!(!response.get(&urgent).unwrap().is_yes(0.5));

    let calls = backend.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].state, state);
    assert_eq!(calls[0].model, "any-model");
    assert_eq!(calls[0].question_ids, ["department", "urgent"]);
}

#[tokio::test]
async fn a_fake_refuses_a_question_it_has_no_answer_for() {
    let mut q = Questions::new();
    q.noul("unknown", "Anything?", None).unwrap();
    let err = fake().answer(&json!({}), "m", &q).await.unwrap_err();
    assert!(
        matches!(&err, Error::MissingAnswer { id, request_id: None } if id == "unknown"),
        "{err}"
    );
}

/// A directory of its own for one test, empty: `judgment-<test>-<pid>`.
fn test_dir(test: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("judgment-{test}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

#[tokio::test]
async fn a_recorder_writes_what_a_replay_answers_offline() {
    let dir = test_dir("a_recorder_writes_what_a_replay_answers_offline");
    let (q, dept, _) = questions();
    let state = json!({ "message": "charged twice" });

    let recorder = Recorder::new(fake(), &dir);
    let live = recorder.answer(&state, "m", &q).await.unwrap();
    assert_eq!(recorder.inner().calls().len(), 1);

    let replay = Replay::open(&dir).unwrap();
    assert_eq!(replay.len(), 1);
    let replayed = replay.answer(&state, "another-alias", &q).await.unwrap();
    assert_eq!(replayed, live);
    assert_eq!(replayed.get(&dept).unwrap().chosen, Department::Billing);

    // Same questions, different state: nobody recorded it.
    let err = replay
        .answer(&json!({ "message": "refund please" }), "m", &q)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::NoRecording(_)), "{err}");

    // The trait is usable as an object, whichever backend is behind it.
    let dynamic: Box<dyn SystemOne> = Box::new(replay);
    assert!(dynamic.answer(&state, "m", &q).await.is_ok());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_committed_recordings_decode_and_rewrite_byte_for_byte() {
    // The 40 Laya responses the benchmark example replays. The Recorder
    // wrote the decoded Response, so none of Laya's extras are in them. The
    // tolerant decoder must read them as the strict one did, and writing one
    // back must give the same bytes, so a recording read and written again
    // does not change.
    let recordings =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/typed-decisions/recordings");
    let out = std::env::temp_dir().join(format!(
        "judgment-the_committed_recordings_decode_and_rewrite_byte_for_byte-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&out);
    let mut seen = 0;
    for entry in std::fs::read_dir(&recordings).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|e| e != "json") {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap();
        let recording: Recording = serde_json::from_str(&text).unwrap();
        let name = path.display();
        assert!(
            recording.response.extra.is_empty(),
            "{name}: {:?}",
            recording.response.extra
        );
        for (id, answer) in &recording.response.answers {
            assert!(
                !matches!(answer, Answer::Unknown(_)),
                "{name}: {id} is {answer:?}"
            );
        }
        let written = write_recording(&out, &recording).unwrap();
        assert_eq!(std::fs::read_to_string(&written).unwrap(), text, "{name}");
        seen += 1;
    }
    assert_eq!(seen, 40);
    assert_eq!(Replay::open(&recordings).unwrap().len(), 40);
    let _ = std::fs::remove_dir_all(&out);
}

#[tokio::test]
async fn a_fake_refuses_a_scripted_option_its_question_does_not_offer() {
    let (q, _, _) = questions();
    let fake = Fake::new()
        .choice("department", [("billing", 0.6), ("sales", 0.4)], 0.5)
        .unwrap()
        .noul("urgent", 0.2)
        .unwrap();
    let err = fake.answer(&json!({}), "m", &q).await.unwrap_err();
    assert!(
        matches!(&err, Error::UnknownOption { id, option, request_id: None }
            if id == "department" && option == "sales"),
        "{err:?}"
    );
    assert!(err.is_unfit());
}

#[tokio::test]
async fn a_fake_refuses_a_scripted_answer_of_another_kind() {
    let (q, _, _) = questions();
    // A Score scripted for a Noul question: the kind is reported, not the
    // (empty) legend.
    let fake = Fake::new()
        .choice("department", [("billing", 1.0)], 1.0)
        .unwrap()
        .score("urgent", [0.2, 0.8], 0.6)
        .unwrap();
    let err = fake.answer(&json!({}), "m", &q).await.unwrap_err();
    assert!(
        matches!(&err, Error::AnswerTypeMismatch { id, expected: "noul", actual, .. }
            if id == "urgent" && actual == "score"),
        "{err:?}"
    );
}

#[tokio::test]
async fn a_fake_that_refuses_a_script_records_no_call() {
    let (q, _, _) = questions();
    let fake = Fake::new()
        .choice("department", [("sales", 1.0)], 1.0)
        .unwrap()
        .noul("urgent", 0.2)
        .unwrap();
    assert!(fake.answer(&json!({}), "m", &q).await.is_err());
    assert!(fake.calls().is_empty(), "{:?}", fake.calls());
    // A missing script is refused the same way, and not recorded either.
    let mut other = Questions::new();
    other.noul("absent", "Anything?", None).unwrap();
    assert!(fake.answer(&json!({}), "m", &other).await.is_err());
    assert!(fake.calls().is_empty(), "{:?}", fake.calls());
}

#[tokio::test]
async fn a_fake_score_echoes_the_levels_of_the_question_it_answers() {
    let mut q = Questions::new();
    let structured = json!({ "what": "cosmetic", "examples": ["a typo"] });
    let severity = q
        .score(
            "severity",
            "How severe is `message`?",
            vec![structured.clone(), json!("degraded"), json!("blocked")],
        )
        .unwrap();
    // Fewer probabilities than levels: the rest read as zero.
    let fake = Fake::new().score("severity", [0.25, 0.75], 0.5).unwrap();
    let response = fake.answer(&json!({}), "m", &q).await.unwrap();
    let Answer::Score {
        score,
        legend,
        probabilities,
        ..
    } = &response.answers["severity"]
    else {
        panic!("{:?}", response.answers["severity"]);
    };
    assert_eq!(
        legend,
        &BTreeMap::from([
            ("0".to_owned(), structured),
            ("1".to_owned(), json!("degraded")),
            ("2".to_owned(), json!("blocked")),
        ]),
        "the legend is the question's levels, as sent"
    );
    assert_eq!(probabilities.len(), 2);
    assert!((score - 0.75).abs() < 1e-12, "{score}");
    let read = response.get(&severity).unwrap();
    assert_eq!(read.levels[1], "degraded");
    assert!(read.probabilities[2].value().abs() < f64::EPSILON);
    assert_eq!(read.nearest_label(), "degraded");
}

#[tokio::test]
async fn a_fake_score_that_sums_past_one_leaves_the_scale() {
    let mut q = Questions::new();
    q.score("impact", "How bad?", ["none", "minor", "major", "outage"])
        .unwrap();
    let fake = Fake::new()
        .score("impact", [0.0, 0.0, 1.0, 1.0], 1.0)
        .unwrap();
    let err = fake.answer(&json!({}), "m", &q).await.unwrap_err();
    assert_eq!(
        err.to_string(),
        r#"answer "impact" does not fit its question: score 5 is outside 0..=3, the scale the question sent"#
    );
    // More probabilities than levels is a key that is not a level.
    let fake = Fake::new()
        .score("impact", [0.2, 0.2, 0.2, 0.2, 0.2], 1.0)
        .unwrap();
    let err = fake.answer(&json!({}), "m", &q).await.unwrap_err();
    assert!(
        matches!(&err, Error::InvalidAnswer { reason, .. }
            if reason == r#"probability key "4" is not a level of its question"#),
        "{err:?}"
    );
}

/// A backend that returns whatever response it was built with, verified or
/// not: what a third-party backend that ignores the contract could do.
struct Unverified(Response);

impl SystemOne for Unverified {
    fn answer<'a>(
        &'a self,
        _state: &'a Value,
        _model: &'a str,
        _questions: &'a Questions,
    ) -> BoxFuture<'a, judgment::Result<Response>> {
        Box::pin(async move { Ok(self.0.clone()) })
    }
}

/// A response to `questions()` that names a department nobody offered.
fn off_list() -> Response {
    serde_json::from_value(json!({
        "model": "third-party",
        "answers": {
            "department": { "type": "choice", "choice": "sales",
                            "probabilities": { "sales": 1.0 }, "confidence": 1.0 },
            "urgent": { "type": "noul", "noul": 0.1 }
        },
        "usage": { "input_tokens": 3, "output_tokens": 1 },
        "request_id": "req_off_list"
    }))
    .unwrap()
}

#[tokio::test]
async fn a_replay_refuses_a_recording_that_does_not_fit() {
    let dir = test_dir("a_replay_refuses_a_recording_that_does_not_fit");
    let (q, _, _) = questions();
    let state = json!({ "message": "charged twice" });
    // A recording written by hand (or by an older release that did not
    // check), filed under the request's hash.
    let hash = request_hash(&state, &q);
    write_recording(
        &dir,
        &Recording {
            case: hash.clone(),
            response: off_list(),
            elapsed_ms: 1,
            request_hash: Some(hash),
        },
    )
    .unwrap();
    let replay = Replay::open(&dir).unwrap();
    assert_eq!(replay.len(), 1);
    let err = replay.answer(&state, "m", &q).await.unwrap_err();
    assert!(
        matches!(&err, Error::UnknownOption { id, option, .. }
            if id == "department" && option == "sales"),
        "{err:?}"
    );
    assert_eq!(
        err.request_id(),
        Some("req_off_list"),
        "the recorded call's id"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn a_recorder_writes_nothing_for_a_response_that_does_not_fit() {
    let dir = test_dir("a_recorder_writes_nothing_for_a_response_that_does_not_fit");
    let (q, _, _) = questions();
    let recorder = Recorder::new(Unverified(off_list()), &dir);
    let err = recorder
        .answer(&json!({ "message": "charged twice" }), "m", &q)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::UnknownOption { .. }), "{err:?}");
    assert_eq!(err.request_id(), Some("req_off_list"));
    let written = std::fs::read_dir(&dir).map_or(0, Iterator::count);
    assert_eq!(written, 0, "nothing is written under {}", dir.display());
    let _ = std::fs::remove_dir_all(&dir);
}

/// Build the crate's questions from one benchmark case's wire JSON, the way
/// `examples/typed_decisions.rs` does, so the request hash is the one the
/// recordings were filed under.
fn questions_from_wire(raw: &serde_json::Map<String, Value>) -> Questions {
    let mut questions = Questions::new();
    for (id, q) in raw {
        let instructions = q.get("instructions").cloned().unwrap_or(Value::Null);
        match q["type"].as_str().unwrap() {
            "noul" => {
                let criteria = q["criteria"].as_object().map(|c| NoulCriteria {
                    yes: c.get("true").cloned(),
                    no: c.get("false").cloned(),
                });
                questions.noul(id.clone(), instructions, criteria).unwrap();
            }
            "choice" => {
                let options = q["criteria"]
                    .as_object()
                    .unwrap()
                    .iter()
                    .map(|(key, desc)| (key.clone(), desc.as_str().map(str::to_owned)));
                questions
                    .dynamic_choice(id.clone(), instructions, options)
                    .unwrap();
            }
            "score" => {
                let levels = q["criteria"].as_array().unwrap().clone();
                questions.score(id.clone(), instructions, levels).unwrap();
            }
            other => panic!("{id}: unknown question type {other}"),
        }
    }
    questions
}

#[tokio::test]
async fn a_replay_of_every_committed_recording_answers_its_sample() {
    // The 40 Laya responses recorded from `sample.jsonl`: each one must
    // still fit the questions it answered, so the benchmark example replays
    // offline through the verifying Replay. Laya echoes every level and
    // offers no option it was not given.
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/typed-decisions");
    let replay = Replay::open(&root.join("recordings")).unwrap();
    let file = std::fs::File::open(root.join("sample.jsonl")).unwrap();
    let mut answered = 0;
    for line in std::io::BufReader::new(file).lines() {
        let line = line.unwrap();
        if line.trim().is_empty() {
            continue;
        }
        let case: Value = serde_json::from_str(&line).unwrap();
        let questions = questions_from_wire(case["questions"].as_object().unwrap());
        let response = replay
            .answer(&case["state"], "typed-decisions", &questions)
            .await
            .unwrap_or_else(|e| panic!("{}: {e}", case["id"]));
        assert_eq!(response.answers.len(), questions.len(), "{}", case["id"]);
        answered += 1;
    }
    assert_eq!(answered, 40);
    assert_eq!(replay.len(), 40);
}
