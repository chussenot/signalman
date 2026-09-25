//! The backends behind one `SystemOne` trait: a `Fake` answers from its
//! table and refuses a question it has no answer for, a `Recorder` writes
//! what another backend answered, and a `Replay` over that directory answers
//! the same request without any backend and refuses a request nobody
//! recorded. The client's implementation is checked in `client.rs`. The
//! committed Laya recordings decode, with nothing unknown and nothing extra,
//! and write back byte for byte.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;

use judgment::eval::{Recording, write_recording};
use judgment::{Answer, Error, Fake, Questions, Recorder, Replay, SystemOne, options};
use serde_json::json;

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
        matches!(&err, Error::MissingAnswer(id) if id == "unknown"),
        "{err}"
    );
}

#[tokio::test]
async fn a_recorder_writes_what_a_replay_answers_offline() {
    let dir = std::env::temp_dir().join(format!("judgment-replay-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
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
