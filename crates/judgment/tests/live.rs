//! Live checks against a real System One server: the wire as a server
//! actually speaks it, not as the mocks in `client.rs` assume it does.
//!
//! Every test is `#[ignore]`, so `cargo test` stays offline; run them by
//! hand against a server:
//!
//! ```sh
//! JUDGMENT_LIVE_BASE_URL=http://127.0.0.1:8000 JUDGMENT_LIVE_MODEL=typed-decisions \
//!   cargo test -p judgment --test live -- --ignored --nocapture
//! ```
//!
//! `JUDGMENT_LIVE_API_KEY` defaults to `unused`, which a server that does not
//! check keys accepts. The bearer test needs a second server that does:
//! `JUDGMENT_LIVE_AUTH_BASE_URL` and `JUDGMENT_LIVE_AUTH_API_KEY`; it skips
//! when they are unset.
//!
//! What was learned running these against Laya is written up in
//! `docs/judgment-laya-typed-decisions.md`.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::print_stderr)]

use std::time::Duration;

use judgment::{Client, Error, NoulCriteria, Questions, Recorder, Replay, SystemOne, options};
use serde_json::json;

options! {
    enum Department {
        Billing = "billing" => "Payments, invoicing, refunds",
        Technical = "technical" => "Bugs, outages, integrations",
        NoneOfThese = "none_of_these" => "No team above fits",
    }
}

const STATE_PAYOUTS: &str =
    "My payouts have been failing for 3 days and nobody answers my tickets.";

fn env_or(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_owned())
}

/// The client under test, or a message that says how to point it somewhere.
fn client() -> Client {
    let base_url = std::env::var("JUDGMENT_LIVE_BASE_URL")
        .expect("set JUDGMENT_LIVE_BASE_URL to a System One server (see the file comment)");
    Client::builder()
        .base_url(base_url)
        .api_key(env_or("JUDGMENT_LIVE_API_KEY", "unused"))
        .model(env_or("JUDGMENT_LIVE_MODEL", "typed-decisions"))
        // CPU inference answers in seconds; the default assumes a hosted API.
        .timeout(Duration::from_secs(120))
        .build()
        .unwrap()
}

fn sum(values: impl IntoIterator<Item = f64>) -> f64 {
    values.into_iter().sum()
}

#[tokio::test]
#[ignore = "needs a live server: JUDGMENT_LIVE_BASE_URL"]
async fn the_three_primitives_round_trip_through_typed_handles() {
    let mut q = Questions::new();
    let dept = q
        .choice::<Department>("department", "Which team should handle `message`?")
        .unwrap();
    let urgent = q
        .noul(
            "urgent",
            "Does `message` convey urgency?",
            Some(NoulCriteria::new(
                "The customer needs an answer today",
                "It can wait",
            )),
        )
        .unwrap();
    let severity = q
        .score(
            "severity",
            "How severe is the problem in `message`?",
            ["cosmetic", "degraded", "blocked"],
        )
        .unwrap();

    let response = client()
        .system_one(&json!({ "message": STATE_PAYOUTS }), &q)
        .await
        .unwrap();

    assert!(!response.model.is_empty(), "the response names a model");
    assert!(
        response.usage.input_tokens > 0,
        "input tokens are counted: {:?}",
        response.usage
    );
    assert_eq!(response.answers.len(), 3);

    let dept = response.get(&dept).unwrap();
    assert_eq!(dept.probabilities.len(), 3, "one probability per option");
    assert!(
        (sum(dept.probabilities.values().map(|p| p.value())) - 1.0).abs() < 0.01,
        "the choice distribution sums to 1 within rounding: {:?}",
        dept.probabilities
    );
    let top = dept
        .probabilities
        .iter()
        .max_by(|a, b| a.1.value().total_cmp(&b.1.value()))
        .map(|(k, _)| *k)
        .unwrap();
    assert_eq!(
        dept.chosen, top,
        "chosen is the arg max of the distribution"
    );
    assert!((0.0..=1.0).contains(&dept.confidence.value()));

    let urgent = response.get(&urgent).unwrap();
    assert!((0.0..=1.0).contains(&urgent.yes.value()));

    let severity = response.get(&severity).unwrap();
    assert_eq!(
        severity.levels,
        vec!["cosmetic", "degraded", "blocked"],
        "the legend echoes the levels in order"
    );
    assert!(
        (0.0..=2.0).contains(&severity.value),
        "the score lies on the level line: {}",
        severity.value
    );
    assert!((sum(severity.probabilities.iter().map(|p| p.value())) - 1.0).abs() < 0.01);
    assert!(severity.nearest_level() <= 2);

    eprintln!(
        "model {}: department {:?} p={:.3} conf={:.3}; urgent {:.3}; severity {:.2} ({})",
        response.model,
        dept.chosen,
        dept.probabilities[&dept.chosen].value(),
        dept.confidence.value(),
        urgent.yes.value(),
        severity.value,
        severity.nearest_label()
    );
}

#[tokio::test]
#[ignore = "needs a live server: JUDGMENT_LIVE_BASE_URL"]
async fn a_structured_score_level_comes_back_decoded() {
    // The API allows a level described as an object; a server echoes the
    // object in the legend, which a legend typed as strings would refuse.
    let mut q = Questions::new();
    let severity = q
        .score(
            "severity",
            "How severe is the problem in `message`?",
            [
                json!({ "what": "cosmetic", "examples": ["a typo in the invoice footer"] }),
                json!("degraded"),
                json!("blocked"),
            ],
        )
        .unwrap();
    let response = client()
        .system_one(&json!({ "message": STATE_PAYOUTS }), &q)
        .await
        .unwrap();
    let severity = response.get(&severity).unwrap();
    assert_eq!(severity.levels.len(), 3);
    assert!(
        severity.levels[0].contains("\"what\":\"cosmetic\""),
        "a structured level is labelled by its JSON: {:?}",
        severity.levels
    );
    assert_eq!(severity.levels[1], "degraded");
}

#[tokio::test]
#[ignore = "needs a live server: JUDGMENT_LIVE_BASE_URL"]
async fn a_choice_option_without_a_description_is_accepted() {
    let mut q = Questions::new();
    let team = q
        .dynamic_choice(
            "team",
            "Which team should handle `message`?",
            [
                (
                    "billing".to_owned(),
                    Some("Payments and refunds".to_owned()),
                ),
                ("technical".to_owned(), None),
                ("none_of_these".to_owned(), None),
            ],
        )
        .unwrap();
    let response = client()
        .system_one(&json!({ "message": STATE_PAYOUTS }), &q)
        .await
        .unwrap();
    let team = response.get(&team).unwrap();
    assert_eq!(team.probabilities.len(), 3);
    assert!(["billing", "technical", "none_of_these"].contains(&team.chosen.as_str()));
}

#[tokio::test]
#[ignore = "needs a live server: JUDGMENT_LIVE_BASE_URL"]
async fn the_model_list_is_either_served_or_absent() {
    // `GET /v1/models` is observed on TypeSafe's API, not documented, and a
    // compatible server may not have it. Either outcome is acceptable; what
    // is not is anything other than a clean success or a clean 404.
    match client().list_models().await {
        Ok(models) => {
            assert!(!models.is_empty());
            eprintln!(
                "models: {}",
                models
                    .iter()
                    .map(|m| m.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        Err(Error::Http { status: 404, .. }) => eprintln!("GET /v1/models: 404, not served"),
        Err(other) => panic!("unexpected: {other}"),
    }
}

#[tokio::test]
#[ignore = "needs a live server: JUDGMENT_LIVE_BASE_URL"]
async fn a_model_name_the_server_does_not_know_is_still_answered() {
    // A client built for the hosted API sends `jev-latest`; a server that
    // routes by name must not fail the request for it. Which checkpoint
    // answered is the server's business; the response says so in `model`.
    let base_url = std::env::var("JUDGMENT_LIVE_BASE_URL").unwrap();
    let client = Client::builder()
        .base_url(base_url)
        .api_key(env_or("JUDGMENT_LIVE_API_KEY", "unused"))
        .model("jev-latest")
        .timeout(Duration::from_secs(300))
        .build()
        .unwrap();
    let mut q = Questions::new();
    let urgent = q
        .noul("urgent", "Does `message` convey urgency?", None)
        .unwrap();
    let response = client
        .system_one(&json!({ "message": STATE_PAYOUTS }), &q)
        .await
        .unwrap();
    response.get(&urgent).unwrap();
    eprintln!("jev-latest was answered by model {:?}", response.model);
}

#[tokio::test]
#[ignore = "needs a live server: JUDGMENT_LIVE_BASE_URL"]
async fn a_live_answer_replays_offline_from_its_recording() {
    let dir = std::env::temp_dir().join(format!("judgment-live-{}", std::process::id()));
    let mut q = Questions::new();
    let dept = q
        .choice::<Department>("department", "Which team should handle `message`?")
        .unwrap();
    let state = json!({ "message": STATE_PAYOUTS });
    let model = env_or("JUDGMENT_LIVE_MODEL", "typed-decisions");

    let recorder = Recorder::new(client(), &dir);
    let live = recorder.answer(&state, &model, &q).await.unwrap();

    let replay = Replay::open(&dir).unwrap();
    assert_eq!(replay.len(), 1);
    let replayed = replay.answer(&state, &model, &q).await.unwrap();
    assert_eq!(
        replayed, live,
        "the replay is the recorded response, byte for byte"
    );
    assert_eq!(
        replayed.get(&dept).unwrap().chosen,
        live.get(&dept).unwrap().chosen
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
#[ignore = "needs a live server: JUDGMENT_LIVE_BASE_URL"]
async fn the_builder_refuses_what_the_wire_would_reject() {
    // Checked before any request: no server involved, but listed here so a
    // live run shows the limits next to the behaviour they guard.
    let mut q = Questions::new();
    let too_many = (0..256).map(|i| (format!("opt{i}"), None));
    let err = q.dynamic_choice("c", "pick", too_many).unwrap_err();
    assert!(matches!(err, Error::InvalidQuestion { .. }), "{err}");
    let err = q.score("s", "rate", ["only one"]).unwrap_err();
    assert!(matches!(err, Error::InvalidQuestion { .. }), "{err}");
    let err = q
        .score("s", "rate", [json!("low"), json!(null)])
        .unwrap_err();
    assert!(matches!(err, Error::InvalidQuestion { .. }), "{err}");
}

#[tokio::test]
#[ignore = "needs an authenticating server: JUDGMENT_LIVE_AUTH_BASE_URL"]
async fn a_wrong_bearer_token_is_unauthorized_and_the_right_one_is_not() {
    let Ok(base_url) = std::env::var("JUDGMENT_LIVE_AUTH_BASE_URL") else {
        eprintln!("skipped: JUDGMENT_LIVE_AUTH_BASE_URL is not set");
        return;
    };
    let key = std::env::var("JUDGMENT_LIVE_AUTH_API_KEY")
        .expect("JUDGMENT_LIVE_AUTH_API_KEY goes with JUDGMENT_LIVE_AUTH_BASE_URL");
    let build = |key: &str| {
        Client::builder()
            .base_url(base_url.clone())
            .api_key(key)
            .model(env_or("JUDGMENT_LIVE_MODEL", "typed-decisions"))
            .timeout(Duration::from_secs(120))
            .build()
            .unwrap()
    };
    let mut q = Questions::new();
    let urgent = q
        .noul("urgent", "Does `message` convey urgency?", None)
        .unwrap();
    let state = json!({ "message": STATE_PAYOUTS });

    let err = build("not-the-key")
        .system_one(&state, &q)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Unauthorized), "{err}");

    let response = build(&key).system_one(&state, &q).await.unwrap();
    response.get(&urgent).unwrap();
}
