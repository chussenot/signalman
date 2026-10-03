//! Speculative fan-out: send every question the decision tree might need in
//! one request, and let the code decide which answers matter.
//!
//! The pattern, from <https://docs.typesafe.ai/patterns/fan-out>. A support
//! ticket needs a category. If it is a bug report it also needs a severity
//! and whether it has steps to reproduce; if it is about billing, whether a
//! refund is asked for. Asking the category first and the rest in a second
//! call costs a round trip per branch. A System One model evaluates every
//! question of a request in parallel, so asking all five at once costs
//! about the latency of asking one (25 Nouls came back in 344 ms from the
//! hosted API, `docs/verification/hosted-typesafe.md`), and the answers a branch
//! does not take are simply not read. Extra questions do cost input tokens;
//! each request's usage is printed so that cost stays visible.
//!
//! In this crate the five questions are five typed handles on one
//! `Questions`. The routing reads only the handles its branch needs, and
//! each handle's type says what it yields: a `Choice` over the `Category`
//! enum, a `Score` on three levels, a `Noul`.
//!
//! ```sh
//! cargo run -p judgment --example fan_out             # replay the committed recordings: no key, no network
//! cargo run -p judgment --example fan_out -- --live   # the hosted API: TYPESAFE_API_KEY, optional TYPESAFE_BASE_URL and TYPESAFE_MODEL
//! cargo run -p judgment --example fan_out -- --record # live, and rewrite the recordings this example replays
//! ```
//!
//! The recordings under `examples/fan-out/recordings/` are `jev-1.13.0`'s
//! answers of 2026-10-03, one file per ticket, keyed by a hash of the state
//! and the questions. `cargo test -p judgment` runs the test at the end of
//! this file, which replays them and checks every route, so a change to a
//! question or a ticket here fails until `--record` is run again. The
//! thresholds are the documentation page's starting points, not values
//! validated on labelled tickets; the page itself says to tune them on your
//! own data, and identical requests to the hosted API can differ by a few
//! hundredths (`docs/verification/hosted-typesafe.md`), so a threshold set exactly
//! on an observed value will flip.

#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::error::Error;
use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

use judgment::{
    Choice, Client, Handle, Noul, Options, Questions, Recorder, Replay, Score, SystemOne, Usage,
    options,
};
use serde_json::{Value, json};

/// The recordings this example replays, next to it.
const RECORDINGS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/examples/fan-out/recordings");

options! {
    /// The broad category of a ticket: the documentation page's four options
    /// and a catch-all. A Choice can only pick from what it is offered, so
    /// without `other` a ticket that fits none of the four is forced into
    /// one of them.
    enum Category {
        BugReport = "bug_report" => "The user is reporting something that is broken or producing errors",
        Billing = "billing" => "Charges, invoices, refunds, subscriptions",
        FeatureRequest = "feature_request" => "The user is requesting new functionality",
        Account = "account" => "Login, permissions, profile, security",
        Other = "other" => "None of the above",
    }
}

/// The five questions of one request, each as the typed handle its branch
/// reads it through.
struct Triage {
    questions: Questions,
    category: Handle<Choice<Category>>,
    bug_severity: Handle<Score>,
    has_reproducible_steps: Handle<Noul>,
    refund_requested: Handle<Noul>,
    frustration: Handle<Score>,
}

impl Triage {
    fn new() -> judgment::Result<Self> {
        let mut questions = Questions::new();
        let category = questions.choice::<Category>(
            "category",
            "Determine the broad category of the support ticket in `ticket.message`",
        )?;
        // Speculative: read only when the category is a bug report.
        let bug_severity = questions.score(
            "bug_severity",
            "How severe is the issue reported in `ticket.message`?",
            [
                "Cosmetic; no impact to functionality",
                "Broken or degraded feature; workaround exists",
                "Blocking issue; no workaround exists",
            ],
        )?;
        let has_reproducible_steps = questions.noul(
            "has_reproducible_steps",
            "The user describes, in `ticket.message`, specific steps to reproduce the issue",
            None,
        )?;
        // Speculative: read only when the category is billing.
        let refund_requested = questions.noul(
            "refund_requested",
            "The user is explicitly asking, in `ticket.message`, for a refund or credit",
            None,
        )?;
        // Read whatever the category.
        let frustration = questions.score(
            "frustration",
            "How frustrated does the user of `ticket.message` appear?",
            ["Calm, matter-of-fact", "Frustrated but civil", "Very angry"],
        )?;
        Ok(Self {
            questions,
            category,
            bug_severity,
            has_reproducible_steps,
            refund_requested,
            frustration,
        })
    }
}

/// Where a ticket goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Route {
    EscalateToEngineering,
    BugBacklog,
    BillingRefundLikely,
    Billing,
    FeatureRequestLog,
    AccountSupport,
    GeneralQueue,
}

/// The documentation page's routing, thresholds included. Each branch reads
/// the answers it needs and no other; a Noul is thresholded on its
/// probability directly, since it carries no separate confidence.
fn route(category: &Choice<Category>, severity: &Score, repro: Noul, refund: Noul) -> Route {
    match category.chosen {
        Category::BugReport if severity.value > 1.5 && repro.yes.value() > 0.6 => {
            Route::EscalateToEngineering
        }
        Category::BugReport => Route::BugBacklog,
        Category::Billing if refund.yes.value() > 0.7 => Route::BillingRefundLikely,
        Category::Billing => Route::Billing,
        Category::FeatureRequest => Route::FeatureRequestLog,
        Category::Account => Route::AccountSupport,
        Category::Other => Route::GeneralQueue,
    }
}

/// Frustration is read whatever the category: above this it earns a
/// priority response on top of its route.
const PRIORITY_ABOVE: f64 = 1.5;

/// One ticket, as the state the questions are asked about.
struct Ticket {
    id: &'static str,
    message: &'static str,
}

/// Four tickets: the documentation page's own (which mixes a double charge,
/// a login failure and a feature idea in one message), and one per branch.
const TICKETS: [Ticket; 4] = [
    Ticket {
        id: "T-98423",
        message: "Hi, I placed an order (#98423) last Thursday and was charged twice. I also can't log in after the site update, and adding Apple Pay would be really helpful. This is getting frustrating.",
    },
    Ticket {
        id: "T-10021",
        message: "Every time I open the Reports tab and pick a date range longer than 30 days the page shows a spinner forever. Steps: 1) Reports 2) Custom range 3) choose 1 Jan to 1 Mar 4) Apply. Happens on Chrome and Firefox. We cannot close the quarter without it.",
    },
    Ticket {
        id: "T-10022",
        message: "Would be great if the export could include a CSV option alongside PDF. Not urgent, just a nice to have.",
    },
    Ticket {
        id: "T-10023",
        message: "This is the THIRD month you have billed me after I cancelled. Refund all three charges immediately or I am disputing them with my bank.",
    },
];

fn state(ticket: &Ticket) -> Value {
    json!({ "ticket": { "id": ticket.id, "message": ticket.message } })
}

/// Everything one request produced, kept so the report can show which
/// answers the route read and which it ignored.
struct Outcome {
    ticket: &'static Ticket,
    category: Choice<Category>,
    bug_severity: Score,
    has_reproducible_steps: Noul,
    refund_requested: Noul,
    frustration: Score,
    route: Route,
    priority: bool,
    usage: Usage,
}

/// Ask the five questions about every ticket and route each one.
async fn run(backend: &dyn SystemOne, model: &str) -> Result<Vec<Outcome>, Box<dyn Error>> {
    let triage = Triage::new()?;
    let mut outcomes = Vec::with_capacity(TICKETS.len());
    for ticket in &TICKETS {
        let response = backend
            .answer(&state(ticket), model, &triage.questions)
            .await?;
        let category = response.get(&triage.category)?;
        let bug_severity = response.get(&triage.bug_severity)?;
        let has_reproducible_steps = response.get(&triage.has_reproducible_steps)?;
        let refund_requested = response.get(&triage.refund_requested)?;
        let frustration = response.get(&triage.frustration)?;
        let route = route(
            &category,
            &bug_severity,
            has_reproducible_steps,
            refund_requested,
        );
        let priority = frustration_is_priority(&frustration);
        outcomes.push(Outcome {
            ticket,
            category,
            bug_severity,
            has_reproducible_steps,
            refund_requested,
            frustration,
            route,
            priority,
            usage: response.usage,
        });
    }
    Ok(outcomes)
}

fn frustration_is_priority(frustration: &Score) -> bool {
    frustration.value > PRIORITY_ABOVE
}

/// The distribution of a Choice, options in their declared order.
fn distribution<O: Options>(choice: &Choice<O>) -> String {
    O::ALL
        .iter()
        .map(|option| format!("{} {:.2}", option.key(), choice.probability_of(option)))
        .collect::<Vec<_>>()
        .join(", ")
}

fn print_outcome(outcome: &Outcome) {
    let Outcome {
        ticket,
        category,
        bug_severity,
        has_reproducible_steps,
        refund_requested,
        frustration,
        route,
        priority,
        usage,
    } = outcome;
    let bug = category.chosen == Category::BugReport;
    let billing = category.chosen == Category::Billing;
    let read = |relevant: bool, branch: &str| {
        if relevant {
            "read".to_owned()
        } else {
            format!("not read: not {branch}")
        }
    };
    println!("\n{}  {}", ticket.id, excerpt(ticket.message));
    println!(
        "  category                {}  confidence {:.2}  ({})",
        category.chosen.key(),
        category.confidence.value(),
        distribution(category)
    );
    println!(
        "  bug_severity            {:.2}  {:?}  confidence {:.2}  {}",
        bug_severity.value,
        bug_severity.nearest_label(),
        bug_severity.confidence.value(),
        read(bug, "a bug report")
    );
    println!(
        "  has_reproducible_steps  {:.2}  {}",
        has_reproducible_steps.yes.value(),
        read(bug, "a bug report")
    );
    println!(
        "  refund_requested        {:.2}  {}",
        refund_requested.yes.value(),
        read(billing, "billing")
    );
    println!(
        "  frustration             {:.2}  {:?}  confidence {:.2}  read whatever the category",
        frustration.value,
        frustration.nearest_label(),
        frustration.confidence.value()
    );
    println!(
        "  -> {route:?}; priority response: {} (frustration {:.2} {} {PRIORITY_ABOVE})",
        if *priority { "yes" } else { "no" },
        frustration.value,
        if *priority { ">" } else { "<=" }
    );
    println!(
        "  one request, five questions: {} input tokens, {} output tokens",
        usage.input_tokens, usage.output_tokens
    );
}

/// The first line's worth of a message.
fn excerpt(message: &str) -> String {
    const WIDTH: usize = 88;
    if message.chars().count() <= WIDTH {
        return format!("{message:?}");
    }
    let cut: String = message.chars().take(WIDTH).collect();
    format!("{:?}", format!("{cut}…"))
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
                        "unexpected argument {other}; usage: fan_out [--live | --record]"
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
    // Start clean, so a recording of a question this example no longer asks
    // does not linger beside the new ones.
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
        "speculative fan-out: {} tickets, five questions each, answers from {label}",
        TICKETS.len()
    );
    for outcome in run(backend.as_ref(), &model).await? {
        print_outcome(&outcome);
    }
    Ok(())
}

#[tokio::main]
async fn main() -> ExitCode {
    match run_cli().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("fan_out: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The committed recordings replay to the routes the documentation
    /// page's thresholds give them. A change to a question or a ticket
    /// above changes the request hash, and this fails with `NoRecording`
    /// until `--record` is run again.
    #[tokio::test]
    async fn the_recordings_replay_to_the_documented_routes() -> Result<(), Box<dyn Error>> {
        let replay = Replay::open(Path::new(RECORDINGS))?;
        let outcomes = run(&replay, "jev-latest").await?;
        let routed: Vec<(&str, Route, bool)> = outcomes
            .iter()
            .map(|o| (o.ticket.id, o.route, o.priority))
            .collect();
        assert_eq!(
            routed,
            [
                // The page's mixed ticket: billing wins (0.73) over the
                // catch-all (0.23), no refund is asked for, and the blocking
                // severity the login failure earned is never read.
                ("T-98423", Route::Billing, false),
                // A blocking bug with numbered steps, but a civil one.
                ("T-10021", Route::EscalateToEngineering, false),
                ("T-10022", Route::FeatureRequestLog, false),
                // The one angry ticket is the one that earns priority.
                ("T-10023", Route::BillingRefundLikely, true),
            ]
        );
        Ok(())
    }
}
