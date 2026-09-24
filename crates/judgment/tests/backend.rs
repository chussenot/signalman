//! The backends behind one `SystemOne` trait: a `Fake` answers from its
//! table and refuses a question it has no answer for, a `Recorder` writes
//! what another backend answered, and a `Replay` over that directory answers
//! the same request without any backend and refuses a request nobody
//! recorded. The client's implementation is checked in `client.rs`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use judgment::{Error, Fake, Questions, Recorder, Replay, SystemOne, options};
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
