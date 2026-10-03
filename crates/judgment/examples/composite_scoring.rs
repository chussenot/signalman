//! Composite scoring: break a judgment into atomic scores, combine them
//! with weights the code owns, and change the weights without asking the
//! model again.
//!
//! The pattern, from <https://docs.typesafe.ai/patterns/composite-scoring>.
//! Ranking resumes for an engineering role is not one judgment; it is
//! several, and which matter depends on the role. Four Score questions rate
//! a resume on Python depth, team leadership, system design and range as a
//! generalist, each on five described levels. Code normalises each to 0–1
//! by dividing by 4 and combines them with one set of weights for a senior
//! individual contributor and another for an engineering manager. The same
//! four answers rank the candidates for both roles, and a third weighting
//! needs no new inference.
//!
//! In this crate that last point is the `Replay` backend: the default run
//! of this example reads recorded answers and never calls the model, and
//! `--weights` adds a column computed from those same answers. A `Score`'s
//! `value` is the probability-weighted level the page divides by 4; its
//! `confidence` says how spread the distribution behind that value is, a
//! flag to read the resume rather than trust the number.
//!
//! ```sh
//! cargo run -p judgment --example composite_scoring                              # replay the committed recordings
//! cargo run -p judgment --example composite_scoring -- --weights 0.5,0.1,0.3,0.1 # a third weighting, no model call
//! cargo run -p judgment --example composite_scoring -- --live                    # the hosted API: TYPESAFE_API_KEY
//! cargo run -p judgment --example composite_scoring -- --record                  # live, and rewrite the recordings
//! ```
//!
//! The recordings under `examples/composite-scoring/recordings/` are
//! `jev-1.13.0`'s answers of 2026-10-03, one per resume, keyed by a hash of
//! the state and the questions. `cargo test -p judgment` runs the test at
//! the end of this file over them. The resumes are invented, and the
//! weights are the documentation page's; a real screen needs labelled
//! outcomes to say whether a ranking was right (`judgment::eval`).

#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::error::Error;
use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

use judgment::{Client, Handle, Questions, Recorder, Replay, Score, SystemOne};
use serde_json::{Value, json};

/// The recordings this example replays, next to it.
const RECORDINGS: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/examples/composite-scoring/recordings"
);

/// Five levels, numbered 0 to 4; a `Score::value` divided by this is on
/// 0–1, as the page normalises it.
const TOP_LEVEL: f64 = 4.0;

/// The four dimensions, one Score each, as the page words them.
struct Dimensions {
    questions: Questions,
    python: Handle<Score>,
    leadership: Handle<Score>,
    design: Handle<Score>,
    generalist: Handle<Score>,
}

impl Dimensions {
    fn new() -> judgment::Result<Self> {
        let mut questions = Questions::new();
        let python = questions.score(
            "python_depth",
            "How much depth of Python experience does this candidate have, based on `resume`?",
            [
                "No Python experience mentioned",
                "Mentioned but no detail",
                "Used in projects, some specifics",
                "Primary language, multiple projects",
                "Deep expertise: architecture, performance, libraries",
            ],
        )?;
        let leadership = questions.score(
            "team_leadership",
            "How much experience does this candidate have managing or leading engineering teams, based on `resume`?",
            [
                "No management experience mentioned",
                "Informal mentorship or tech lead role",
                "Led a small team or project",
                "Managed a team with direct reports",
                "Managed multiple teams or an engineering org",
            ],
        )?;
        let design = questions.score(
            "system_design",
            "How much experience does this candidate have designing large-scale or distributed systems, based on `resume`?",
            [
                "No architecture work mentioned",
                "Contributed to design discussions",
                "Designed components of a larger system",
                "Owned architecture of a significant system",
                "Designed systems at scale across multiple domains",
            ],
        )?;
        let generalist = questions.score(
            "generalist",
            "How much evidence is there in `resume` that this candidate picks up unfamiliar tools, roles, or domains outside their core specialty?",
            [
                "Only one domain or role mentioned",
                "Some variety but within a narrow field",
                "Worked across a few different areas or tech stacks",
                "Regularly moved between domains, wore many hats",
                "Track record of ramping up in unfamiliar areas and delivering",
            ],
        )?;
        Ok(Self {
            questions,
            python,
            leadership,
            design,
            generalist,
        })
    }
}

/// One role's weights over the four normalised dimensions. They sum to 1,
/// so a composite is on 0–1 too.
#[derive(Debug, Clone, Copy)]
struct Weights {
    name: &'static str,
    python: f64,
    leadership: f64,
    design: f64,
    generalist: f64,
}

/// The page's two roles.
const SENIOR_IC: Weights = Weights {
    name: "senior IC",
    python: 0.40,
    leadership: 0.10,
    design: 0.40,
    generalist: 0.10,
};
const ENGINEERING_MANAGER: Weights = Weights {
    name: "eng manager",
    python: 0.15,
    leadership: 0.40,
    design: 0.20,
    generalist: 0.25,
};

/// A candidate's resume, as the state the questions are asked about.
struct Candidate {
    name: &'static str,
    resume: &'static str,
}

/// Three invented resumes that pull in different directions.
const CANDIDATES: [Candidate; 3] = [
    Candidate {
        name: "Priya",
        resume: "Staff engineer, 9 years. Python is my primary language: I designed and own our event ingestion pipeline (Kafka, Python services, ClickHouse) handling 2 billion events a day, led the rewrite that cut p99 latency by 70%, and maintain two internal libraries used by every team. I mentor three engineers and run the architecture review group. No direct reports.",
    },
    Candidate {
        name: "Marcus",
        resume: "Engineering manager, 12 years in software. Managed three teams (18 engineers) across payments and risk for the last five years; before that a Java backend engineer and tech lead. Own the platform roadmap, hiring and the on-call programme; reorganised the group from component teams to stream-aligned teams. Comfortable in Python for scripts and data checks.",
    },
    Candidate {
        name: "Sam",
        resume: "Engineer, 4 years, mostly at an early-stage startup where I did whatever was needed: started in support, moved to QA automation (Python, pytest), then backend (Node, then Go), then owned our infrastructure (Terraform, Kubernetes) when the only infra engineer left. Led a two-person project to migrate billing to Stripe. Learning Rust.",
    },
];

fn state(candidate: &Candidate) -> Value {
    json!({ "resume": candidate.resume })
}

/// One candidate's four scores.
struct Scored {
    candidate: &'static Candidate,
    python: Score,
    leadership: Score,
    design: Score,
    generalist: Score,
}

impl Scored {
    /// The page's step two, first half: each dimension on 0–1.
    fn normalised(&self) -> [f64; 4] {
        [
            self.python.value / TOP_LEVEL,
            self.leadership.value / TOP_LEVEL,
            self.design.value / TOP_LEVEL,
            self.generalist.value / TOP_LEVEL,
        ]
    }

    /// The page's step two, second half: the weighted sum for one role.
    fn composite(&self, weights: &Weights) -> f64 {
        let [python, leadership, design, generalist] = self.normalised();
        weights.python * python
            + weights.leadership * leadership
            + weights.design * design
            + weights.generalist * generalist
    }

    /// The least concentrated of the four distributions: the dimension to
    /// read the resume for rather than trust the number.
    fn least_confident(&self) -> (&'static str, f64) {
        [
            ("python", self.python.confidence.value()),
            ("leadership", self.leadership.confidence.value()),
            ("design", self.design.confidence.value()),
            ("generalist", self.generalist.confidence.value()),
        ]
        .into_iter()
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .unwrap_or(("python", 1.0))
    }
}

/// Ask the four questions about every resume: one request per candidate,
/// the four answered in parallel.
async fn run(backend: &dyn SystemOne, model: &str) -> Result<Vec<Scored>, Box<dyn Error>> {
    let dimensions = Dimensions::new()?;
    let mut scored = Vec::with_capacity(CANDIDATES.len());
    for candidate in &CANDIDATES {
        let response = backend
            .answer(&state(candidate), model, &dimensions.questions)
            .await?;
        scored.push(Scored {
            candidate,
            python: response.get(&dimensions.python)?,
            leadership: response.get(&dimensions.leadership)?,
            design: response.get(&dimensions.design)?,
            generalist: response.get(&dimensions.generalist)?,
        });
    }
    Ok(scored)
}

/// Candidate names, best first, under `weights`.
fn ranking<'a>(scored: &'a [Scored], weights: &Weights) -> Vec<&'a str> {
    let mut ranked: Vec<(&str, f64)> = scored
        .iter()
        .map(|s| (s.candidate.name, s.composite(weights)))
        .collect();
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1));
    ranked.into_iter().map(|(name, _)| name).collect()
}

fn print_report(scored: &[Scored], roles: &[Weights]) {
    println!("\nper dimension: the probability-weighted level on 0–4 (and its confidence)");
    for s in scored {
        let (dimension, confidence) = s.least_confident();
        println!(
            "  {:<7} python {:.2} ({:.2})  leadership {:.2} ({:.2})  design {:.2} ({:.2})  generalist {:.2} ({:.2})   least certain: {dimension} {confidence:.2}",
            s.candidate.name,
            s.python.value,
            s.python.confidence.value(),
            s.leadership.value,
            s.leadership.confidence.value(),
            s.design.value,
            s.design.confidence.value(),
            s.generalist.value,
            s.generalist.confidence.value()
        );
    }
    println!(
        "\ncomposites: each dimension divided by {TOP_LEVEL}, then weighted; the weights are code"
    );
    for role in roles {
        println!(
            "  {:<12} weights python {:.2} leadership {:.2} design {:.2} generalist {:.2}",
            role.name, role.python, role.leadership, role.design, role.generalist
        );
        for s in scored {
            println!("    {:<7} {:.2}", s.candidate.name, s.composite(role));
        }
        println!("    ranking: {}", ranking(scored, role).join(" > "));
    }
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

struct Args {
    mode: Mode,
    /// A third weighting from the command line, `--weights p,l,d,g`.
    custom: Option<Weights>,
}

fn parse_args() -> Result<Args, String> {
    const USAGE: &str = "usage: composite_scoring [--live | --record] [--weights python,leadership,design,generalist]";
    let mut args = std::env::args().skip(1);
    let mut parsed = Args {
        mode: Mode::Replay,
        custom: None,
    };
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--live" => parsed.mode = Mode::Live,
            "--record" => parsed.mode = Mode::Record,
            "--weights" => {
                let spec = args.next().ok_or(USAGE)?;
                parsed.custom = Some(parse_weights(&spec)?);
            }
            other => return Err(format!("unexpected argument {other}; {USAGE}")),
        }
    }
    Ok(parsed)
}

/// `p,l,d,g`, four weights that sum to 1 within rounding.
fn parse_weights(spec: &str) -> Result<Weights, String> {
    let values: Vec<f64> = spec
        .split(',')
        .map(|part| {
            part.trim()
                .parse::<f64>()
                .map_err(|e| format!("--weights {spec}: {e}"))
        })
        .collect::<Result<_, _>>()?;
    let [python, leadership, design, generalist] = values[..] else {
        return Err(format!("--weights {spec}: four values are needed"));
    };
    let total = python + leadership + design + generalist;
    if (total - 1.0).abs() > 0.01 || values.iter().any(|w| *w < 0.0) {
        return Err(format!(
            "--weights {spec}: the weights must be non-negative and sum to 1, not {total}"
        ));
    }
    Ok(Weights {
        name: "custom",
        python,
        leadership,
        design,
        generalist,
    })
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
    let args = parse_args()?;
    let (backend, label) = backend(args.mode)?;
    let model = std::env::var("TYPESAFE_MODEL").unwrap_or_else(|_| "jev-latest".to_owned());
    println!(
        "composite scoring: {} resumes, four Score questions each, answers from {label}",
        CANDIDATES.len()
    );
    let scored = run(backend.as_ref(), &model).await?;
    let mut roles = vec![SENIOR_IC, ENGINEERING_MANAGER];
    if let Some(custom) = args.custom {
        roles.push(custom);
    }
    print_report(&scored, &roles);
    if args.mode == Mode::Replay {
        println!(
            "\nno model was called: a new weighting is a new column over the same recorded answers"
        );
    }
    Ok(())
}

#[tokio::main]
async fn main() -> ExitCode {
    match run_cli().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("composite_scoring: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The committed recordings replay to the rankings the page's weights
    /// give them, and the two roles rank the same three people differently,
    /// which is the point of keeping the dimensions apart.
    #[tokio::test]
    async fn the_recordings_replay_to_two_rankings() -> Result<(), Box<dyn Error>> {
        let replay = Replay::open(Path::new(RECORDINGS))?;
        let scored = run(&replay, "jev-latest").await?;
        assert_eq!(scored.len(), CANDIDATES.len());
        for s in &scored {
            for value in s.normalised() {
                assert!(
                    (0.0..=1.0).contains(&value),
                    "{}: {value}",
                    s.candidate.name
                );
            }
        }
        // Priya leads on Python and design, Marcus on leadership, Sam on
        // range; the weights decide who comes first for which role.
        assert_eq!(ranking(&scored, &SENIOR_IC), ["Priya", "Sam", "Marcus"]);
        assert_eq!(
            ranking(&scored, &ENGINEERING_MANAGER),
            ["Marcus", "Sam", "Priya"]
        );
        Ok(())
    }

    #[test]
    fn weights_parse_and_are_checked() {
        let custom = parse_weights("0.5, 0.1, 0.3, 0.1");
        assert!(custom.is_ok(), "{custom:?}");
        assert!(parse_weights("0.5,0.5").is_err());
        assert!(parse_weights("0.5,0.5,0.5,0.5").is_err());
        assert!(parse_weights("1,0,0,x").is_err());
    }
}
