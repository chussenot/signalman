//! Intent routing: a cheap, fast classifier in front of the expensive
//! handlers, so that deterministic code, a specialist model or a person
//! each get only the requests that need them.
//!
//! The pattern, from <https://docs.typesafe.ai/patterns/intent-routing>.
//! Customer messages arrive and each needs the right handler. Sending every
//! one through a large generative model to find out what it is costs that
//! model's price and latency on every message. One request to a System One
//! model asks two questions instead: the intent, a Choice, and how complex
//! the request is to resolve, a Score. Then code routes: an order-status
//! question goes to a database lookup with no model at all; a product or
//! returns question goes to a specialist model loaded with that domain's
//! context; a complaint goes to a resolution model unless it is complex, or
//! its complexity is itself uncertain, in which case a person takes it; and
//! any message the classifier is not confident about goes to a person too.
//!
//! In this crate the two questions are two typed handles on one
//! `Questions`, the intent a `Choice` over the `Intent` enum and the
//! complexity a `Score`, and the gate reads a `Confidence` off each. The
//! handlers here are stubs that say what they would cost; the pattern is
//! the routing, not the handlers.
//!
//! One thing the recorded answers show about the second gate. A Score's
//! confidence falls fast when probability splits between neighbouring
//! levels (a 60/40 split on three levels is 0.40, the confidence page's
//! formula), and `jev-1.13.0` answered mild complaints with just such a
//! split between "simple" and "requires some judgment", so the page's 0.5
//! floor on the complexity's confidence sends them to a person. That floor,
//! or the number of levels, is the first threshold to tune on real
//! messages; the messages below put one complaint on each side of it.
//!
//! ```sh
//! cargo run -p judgment --example intent_routing             # replay the committed recordings: no key, no network
//! cargo run -p judgment --example intent_routing -- --live   # the hosted API: TYPESAFE_API_KEY, optional TYPESAFE_BASE_URL and TYPESAFE_MODEL
//! cargo run -p judgment --example intent_routing -- --record # live, and rewrite the recordings this example replays
//! ```
//!
//! The recordings under `examples/intent-routing/recordings/` are
//! `jev-1.13.0`'s answers of 2026-10-03, one per message, keyed by a hash of
//! the state and the questions. `cargo test -p judgment` runs the test at
//! the end of this file over them. The thresholds are the documentation
//! page's starting points, to be tuned on your own messages.

#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::error::Error;
use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

use judgment::{
    Choice, Client, Handle, Options, Questions, Recorder, Replay, Score, SystemOne, options,
};
use serde_json::{Value, json};

/// The recordings this example replays, next to it.
const RECORDINGS: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/examples/intent-routing/recordings"
);

options! {
    /// The primary intent of a message: the documentation page's four and a
    /// catch-all, so a message about none of them is not forced into one.
    enum Intent {
        OrderStatus = "order_status" => "Asking about an existing order",
        ProductQuestion = "product_question" => "Asking about a product before buying",
        ReturnExchange = "return_exchange" => "Wants to return or exchange something",
        Complaint = "complaint" => "Unhappy with experience, wants resolution",
        Other = "other" => "None of the above",
    }
}

/// The two questions of one request.
struct Classifier {
    questions: Questions,
    intent: Handle<Choice<Intent>>,
    complexity: Handle<Score>,
}

impl Classifier {
    fn new() -> judgment::Result<Self> {
        let mut questions = Questions::new();
        let intent =
            questions.choice::<Intent>("intent", "The primary intent of the customer `message`")?;
        let complexity = questions.score(
            "complexity",
            "How complex is the request in `message` to resolve?",
            [
                "Simple lookup or standard procedure",
                "Requires some judgment or multi-step process",
                "Unusual situation, edge case, or escalation needed",
            ],
        )?;
        Ok(Self {
            questions,
            intent,
            complexity,
        })
    }
}

/// Below this intent confidence a person takes the message, whatever the
/// intent; below it on the complexity, a complaint is not automated.
const CONFIDENCE_FLOOR: f64 = 0.5;
/// A complaint above this complexity (the scale's middle level) is not
/// automated either.
const COMPLAINT_AUTOMATED_UP_TO: f64 = 1.0;

/// What handles a message, and what that costs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Handler {
    /// A database lookup: deterministic code, no model.
    OrderLookup,
    /// A generative model loaded with the product catalogue.
    ProductSpecialist,
    /// A generative model loaded with the returns policy.
    ReturnsSpecialist,
    /// A generative model loaded with the resolution playbook.
    ComplaintResolution,
    /// A person.
    HumanAgent,
}

impl Handler {
    fn cost(self) -> &'static str {
        match self {
            Self::OrderLookup => "deterministic code, no model",
            Self::ProductSpecialist | Self::ReturnsSpecialist | Self::ComplaintResolution => {
                "one generative-model call"
            }
            Self::HumanAgent => "a person's time",
        }
    }
}

/// The documentation page's routing: a confidence floor on the intent,
/// then a handler per intent, with the complaint branch gated a second
/// time on the complexity and on how sure the model is of that complexity.
fn route(intent: &Choice<Intent>, complexity: &Score) -> (Handler, &'static str) {
    if intent.confidence.value() < CONFIDENCE_FLOOR {
        return (
            Handler::HumanAgent,
            "intent confidence below the floor: not classified",
        );
    }
    match intent.chosen {
        Intent::OrderStatus => (Handler::OrderLookup, "a lookup answers it"),
        Intent::ProductQuestion => (Handler::ProductSpecialist, "needs the catalogue"),
        Intent::ReturnExchange => (Handler::ReturnsSpecialist, "needs the returns policy"),
        Intent::Complaint if complexity.value > COMPLAINT_AUTOMATED_UP_TO => (
            Handler::HumanAgent,
            "a complaint too complex for safe automation",
        ),
        Intent::Complaint if complexity.confidence.value() < CONFIDENCE_FLOOR => (
            Handler::HumanAgent,
            "a complaint whose complexity the model is not sure of",
        ),
        Intent::Complaint => (
            Handler::ComplaintResolution,
            "a complaint the playbook covers",
        ),
        Intent::Other => (Handler::HumanAgent, "no handler for it"),
    }
}

/// One customer message.
struct Message {
    id: &'static str,
    text: &'static str,
}

/// Seven messages: one per handler, a complaint on each side of the
/// complexity gate and one the model is unsure how complex it is, and one
/// that says almost nothing.
const MESSAGES: [Message; 7] = [
    Message {
        id: "M1",
        text: "Hi, where is my order #4471? The tracking page said delivered yesterday but nothing has arrived.",
    },
    Message {
        id: "M2",
        text: "Does the X200 kettle work on 110 V? I am in the US and your site only lists 230 V.",
    },
    Message {
        id: "M3",
        text: "I would like to exchange the medium jacket I bought last week for a large. Tags still on.",
    },
    Message {
        id: "M4",
        text: "The courier left my parcel in the rain and the box was soaked through. The contents seem fine but I am not happy about it.",
    },
    Message {
        id: "M5",
        text: "This is the fourth time I am writing. Two replacements arrived broken, your agent promised a refund that never came, and now my account shows a new charge I never authorised. I want a manager.",
    },
    Message {
        id: "M6",
        text: "k thx bye",
    },
    Message {
        id: "M7",
        text: "I was charged for express shipping but the parcel came by standard post. Please refund the shipping difference.",
    },
];

fn state(message: &Message) -> Value {
    json!({ "message": message.text })
}

struct Outcome {
    message: &'static Message,
    intent: Choice<Intent>,
    complexity: Score,
    handler: Handler,
    reason: &'static str,
}

async fn run(backend: &dyn SystemOne, model: &str) -> Result<Vec<Outcome>, Box<dyn Error>> {
    let classifier = Classifier::new()?;
    let mut outcomes = Vec::with_capacity(MESSAGES.len());
    for message in &MESSAGES {
        let response = backend
            .answer(&state(message), model, &classifier.questions)
            .await?;
        let intent = response.get(&classifier.intent)?;
        let complexity = response.get(&classifier.complexity)?;
        let (handler, reason) = route(&intent, &complexity);
        outcomes.push(Outcome {
            message,
            intent,
            complexity,
            handler,
            reason,
        });
    }
    Ok(outcomes)
}

fn print_outcome(outcome: &Outcome) {
    let Outcome {
        message,
        intent,
        complexity,
        handler,
        reason,
    } = outcome;
    println!("\n{}  {:?}", message.id, message.text);
    println!(
        "  intent      {}  confidence {:.2}  ({})",
        intent.chosen.key(),
        intent.confidence.value(),
        Intent::ALL
            .iter()
            .map(|option| format!("{} {:.2}", option.key(), intent.probability_of(option)))
            .collect::<Vec<_>>()
            .join(", ")
    );
    println!(
        "  complexity  {:.2}  {:?}  confidence {:.2}",
        complexity.value,
        complexity.nearest_label(),
        complexity.confidence.value()
    );
    println!("  -> {handler:?} ({}): {reason}", handler.cost());
}

fn print_summary(outcomes: &[Outcome]) {
    let count =
        |predicate: fn(Handler) -> bool| outcomes.iter().filter(|o| predicate(o.handler)).count();
    let code = count(|h| h == Handler::OrderLookup);
    let model = count(|h| {
        matches!(
            h,
            Handler::ProductSpecialist | Handler::ReturnsSpecialist | Handler::ComplaintResolution
        )
    });
    let people = count(|h| h == Handler::HumanAgent);
    println!(
        "\n{} messages: {code} answered by code, {model} by a specialist model, {people} by a person; one System One request each decided which",
        outcomes.len()
    );
}

/// Where the answers come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// The committed recordings; the default.
    Replay,
    /// The hosted API, or the server `TYPESAFE_BASE_URL` names.
    Live,
    /// Live, writing every answer over the committed recordings.
    Record,
}

impl Mode {
    fn from_args() -> Result<Self, String> {
        let mut mode = Self::Replay;
        for arg in std::env::args().skip(1) {
            match arg.as_str() {
                "--live" => mode = Self::Live,
                "--record" => mode = Self::Record,
                other => {
                    return Err(format!(
                        "unexpected argument {other}; usage: intent_routing [--live | --record]"
                    ));
                }
            }
        }
        Ok(mode)
    }
}

/// The backend for `mode`, and a label for the report.
fn backend(mode: Mode) -> Result<(Box<dyn SystemOne>, String), Box<dyn Error>> {
    if mode == Mode::Replay {
        let replay = Replay::open(Path::new(RECORDINGS)).map_err(|e| {
            format!("{e}: no recordings to replay; run with --record and TYPESAFE_API_KEY set")
        })?;
        let label = format!("the {} recordings under {RECORDINGS}", replay.len());
        return Ok((Box::new(replay), label));
    }
    let mut builder = Client::builder().timeout(Duration::from_secs(30));
    if let Ok(url) = std::env::var("TYPESAFE_BASE_URL") {
        builder = builder.base_url(url);
    }
    let client = builder.build()?;
    if mode == Mode::Live {
        return Ok((Box::new(client), "the live API".to_owned()));
    }
    match std::fs::remove_dir_all(RECORDINGS) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    Ok((
        Box::new(Recorder::new(client, RECORDINGS)),
        format!("the live API, recorded under {RECORDINGS}"),
    ))
}

async fn run_cli() -> Result<(), Box<dyn Error>> {
    let mode = Mode::from_args()?;
    let (backend, label) = backend(mode)?;
    let model = std::env::var("TYPESAFE_MODEL").unwrap_or_else(|_| "jev-latest".to_owned());
    println!(
        "intent routing: {} messages, two questions each, answers from {label}",
        MESSAGES.len()
    );
    let outcomes = run(backend.as_ref(), &model).await?;
    for outcome in &outcomes {
        print_outcome(outcome);
    }
    print_summary(&outcomes);
    Ok(())
}

#[tokio::main]
async fn main() -> ExitCode {
    match run_cli().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("intent_routing: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The committed recordings replay to the handlers the documentation
    /// page's routing gives them.
    #[tokio::test]
    async fn the_recordings_replay_to_the_documented_handlers() -> Result<(), Box<dyn Error>> {
        let replay = Replay::open(Path::new(RECORDINGS))?;
        let outcomes = run(&replay, "jev-latest").await?;
        let handlers: Vec<(&str, Handler)> =
            outcomes.iter().map(|o| (o.message.id, o.handler)).collect();
        assert_eq!(handlers.len(), MESSAGES.len());
        assert_eq!(
            handlers,
            [
                ("M1", Handler::OrderLookup),
                ("M2", Handler::ProductSpecialist),
                ("M3", Handler::ReturnsSpecialist),
                // A complaint split between the first two complexity
                // levels: confidence 0.45, under the floor, so a person.
                ("M4", Handler::HumanAgent),
                // Complexity 2.00 at confidence 1.00: too complex.
                ("M5", Handler::HumanAgent),
                // `other` at 1.00: no handler.
                ("M6", Handler::HumanAgent),
                // Complexity 0.18 at confidence 0.73: the playbook's.
                ("M7", Handler::ComplaintResolution),
            ]
        );
        Ok(())
    }
}
