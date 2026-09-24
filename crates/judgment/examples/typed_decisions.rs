//! Replay the typed-decisions benchmark through the crate and score it.
//!
//! The benchmark (<https://huggingface.co/datasets/LocalLLaMA/typed-decisions>)
//! is 400 test cases, each one state and five typed questions in the System
//! One wire shape with a gold distribution per question. That makes it the
//! cheapest end-to-end check that a backend and this crate agree on the wire:
//! every case exercises the question builder, the request, the answer decoder
//! and the evaluation metrics, and the gold labels turn the run into numbers
//! that can be compared with the model card.
//!
//! ```sh
//! # live, against any server that speaks the wire (here laya-serve, see
//! # docs/judgment-laya-typed-decisions.md), recording every answer
//! TYPESAFE_BASE_URL=http://127.0.0.1:8000 TYPESAFE_MODEL=typed-decisions TYPESAFE_API_KEY=unused \
//!   cargo run -p judgment --example typed_decisions -- \
//!     crates/judgment/examples/typed-decisions/sample.jsonl --record /tmp/laya-run
//!
//! # offline, from the recordings, no server and no key
//! cargo run -p judgment --example typed_decisions -- \
//!   crates/judgment/examples/typed-decisions/sample.jsonl --replay /tmp/laya-run
//! ```
//!
//! `--limit N` stops after N cases; `--json` prints the report as JSON.
//! `export.py` next to `sample.jsonl` produces the full split.

#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::collections::BTreeMap;
use std::io::BufRead;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use judgment::eval::{ECE_BINS, Judgment, Latency, QuestionMetrics};
use judgment::{Client, NoulCriteria, Questions, Recorder, Replay, SystemOne};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One benchmark case, as `export.py` writes it.
#[derive(Deserialize)]
struct Case {
    id: String,
    workflow: String,
    state: Value,
    questions: BTreeMap<String, Value>,
    gold: BTreeMap<String, Gold>,
}

/// The gold answer for one question: the label the annotators agreed on.
#[derive(Deserialize)]
struct Gold {
    label: String,
}

/// Where a judgment came from, for the per-workflow and per-type tables.
struct Graded {
    workflow: String,
    kind: &'static str,
    judgment: Judgment,
}

#[derive(Serialize)]
struct Row {
    n: usize,
    accuracy: Option<f64>,
    brier: Option<f64>,
    ece: Option<f64>,
    confidence_when_right: Option<f64>,
    confidence_when_wrong: Option<f64>,
}

#[derive(Serialize)]
struct Report {
    backend: String,
    model: Option<String>,
    cases: usize,
    failed: usize,
    overall: Row,
    by_type: BTreeMap<String, Row>,
    by_workflow: BTreeMap<String, Row>,
    latency: Latency,
}

struct Args {
    cases: PathBuf,
    record: Option<PathBuf>,
    replay: Option<PathBuf>,
    limit: Option<usize>,
    json: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut args = std::env::args().skip(1);
    let mut parsed = Args {
        cases: PathBuf::new(),
        record: None,
        replay: None,
        limit: None,
        json: false,
    };
    let mut cases = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--record" => {
                parsed.record = Some(args.next().ok_or("--record needs a directory")?.into());
            }
            "--replay" => {
                parsed.replay = Some(args.next().ok_or("--replay needs a directory")?.into());
            }
            "--limit" => {
                parsed.limit = Some(
                    args.next()
                        .ok_or("--limit needs a number")?
                        .parse()
                        .map_err(|e| format!("--limit: {e}"))?,
                );
            }
            "--json" => parsed.json = true,
            other if cases.is_none() => cases = Some(PathBuf::from(other)),
            other => return Err(format!("unexpected argument {other}")),
        }
    }
    parsed.cases = cases.ok_or(
        "usage: typed_decisions <cases.jsonl> [--record DIR | --replay DIR] [--limit N] [--json]",
    )?;
    if parsed.record.is_some() && parsed.replay.is_some() {
        return Err("--record and --replay are exclusive".into());
    }
    Ok(parsed)
}

/// Build the crate's typed questions from a case's wire JSON. The benchmark
/// is written in the wire shape, so this is the one place the example
/// interprets it; the builder then enforces the crate's own limits.
fn questions_from_wire(
    raw: &BTreeMap<String, Value>,
) -> judgment::Result<(Questions, BTreeMap<String, &'static str>)> {
    let mut questions = Questions::new();
    let mut kinds = BTreeMap::new();
    for (id, q) in raw {
        let instructions = q.get("instructions").cloned().unwrap_or(Value::Null);
        let kind = q.get("type").and_then(Value::as_str).unwrap_or_default();
        match kind {
            "noul" => {
                let criteria = q
                    .get("criteria")
                    .and_then(Value::as_object)
                    .map(|c| NoulCriteria {
                        yes: c.get("true").cloned(),
                        no: c.get("false").cloned(),
                    });
                questions.noul(id.clone(), instructions, criteria)?;
                kinds.insert(id.clone(), "noul");
            }
            "choice" => {
                let options = q
                    .get("criteria")
                    .and_then(Value::as_object)
                    .into_iter()
                    .flatten()
                    .map(|(key, desc)| (key.clone(), desc.as_str().map(str::to_owned)));
                questions.dynamic_choice(id.clone(), instructions, options)?;
                kinds.insert(id.clone(), "choice");
            }
            "score" => {
                let levels = q
                    .get("criteria")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                questions.score(id.clone(), instructions, levels)?;
                kinds.insert(id.clone(), "score");
            }
            other => {
                return Err(judgment::Error::InvalidQuestion {
                    id: id.clone(),
                    reason: format!("unknown question type {other:?}"),
                });
            }
        }
    }
    Ok((questions, kinds))
}

/// Grade one wire answer against the benchmark's label.
///
/// The confidence graded for calibration is the probability of the reported
/// answer, on every type: for a Noul `max(p, 1 - p)`, for a Choice or Score
/// the probability of the option or level chosen. The wire's `confidence`
/// field is not used here because it is not a probability that the answer is
/// right: TypeSafe computes it from the spread of the distribution and Laya
/// as one minus the normalised entropy, so an ECE over it would measure
/// neither model's calibration and the two would not compare. The
/// benchmark's own ECE and the model card's gating advice both use the
/// answer's probability.
fn grade(answer: &judgment::Answer, expected: Option<&str>) -> Judgment {
    // The benchmark labels a noul "true" or "false".
    if let judgment::Answer::Noul { noul } = answer {
        return Judgment::noul(noul.value(), expected.map(|l| l == "true"), true);
    }
    let graded = Judgment::of_answer(answer, expected);
    let answer_probability = graded
        .probabilities
        .get(&graded.predicted)
        .copied()
        .unwrap_or_default();
    Judgment::new(
        graded.predicted,
        graded.expected,
        answer_probability,
        graded.probabilities,
        true,
    )
}

fn row<'a>(judgments: impl IntoIterator<Item = &'a Judgment>) -> Row {
    let m = QuestionMetrics::summarise(judgments, ECE_BINS);
    Row {
        n: m.labelled,
        accuracy: m.accuracy,
        brier: m.brier,
        ece: m.ece,
        confidence_when_right: m.confidence_when_right,
        confidence_when_wrong: m.confidence_when_wrong,
    }
}

fn fmt(v: Option<f64>) -> String {
    v.map_or_else(|| "-".to_owned(), |v| format!("{v:.3}"))
}

fn print_table(title: &str, rows: &BTreeMap<String, Row>) {
    println!(
        "\n{title:<28} {:>5} {:>9} {:>7} {:>7} {:>9} {:>9}",
        "n", "accuracy", "brier", "ece", "conf ok", "conf ko"
    );
    for (name, r) in rows {
        println!(
            "{name:<28} {:>5} {:>9} {:>7} {:>7} {:>9} {:>9}",
            r.n,
            fmt(r.accuracy),
            fmt(r.brier),
            fmt(r.ece),
            fmt(r.confidence_when_right),
            fmt(r.confidence_when_wrong)
        );
    }
}

fn read_cases(path: &PathBuf, limit: Option<usize>) -> Result<Vec<Case>, String> {
    let file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut cases = Vec::new();
    for (n, line) in std::io::BufReader::new(file).lines().enumerate() {
        let line = line.map_err(|e| format!("{}:{}: {e}", path.display(), n + 1))?;
        if line.trim().is_empty() {
            continue;
        }
        cases.push(
            serde_json::from_str(&line)
                .map_err(|e| format!("{}:{}: {e}", path.display(), n + 1))?,
        );
        if limit.is_some_and(|l| cases.len() >= l) {
            break;
        }
    }
    Ok(cases)
}

async fn run(args: Args) -> Result<Report, String> {
    let cases = read_cases(&args.cases, args.limit)?;
    let model = std::env::var("TYPESAFE_MODEL").unwrap_or_else(|_| "typed-decisions".to_owned());

    // One `dyn SystemOne` for all three modes: the code that asks and grades
    // never learns whether the answers are live, being recorded, or replayed.
    let (backend, label): (Box<dyn SystemOne>, String) = if let Some(dir) = &args.replay {
        let replay = Replay::open(dir).map_err(|e| e.to_string())?;
        (Box::new(replay), format!("replay {}", dir.display()))
    } else {
        let mut builder = Client::builder()
            .model(model.clone())
            // A CPU-bound backend answers five questions in seconds, not the
            // hundreds of milliseconds the default timeout assumes.
            .timeout(Duration::from_secs(120));
        if let Ok(url) = std::env::var("TYPESAFE_BASE_URL") {
            builder = builder.base_url(url);
        }
        let client = builder.build().map_err(|e| e.to_string())?;
        match &args.record {
            Some(dir) => (
                Box::new(Recorder::new(client, dir.clone())),
                format!("live, recording to {}", dir.display()),
            ),
            None => (Box::new(client), "live".to_owned()),
        }
    };

    let mut graded: Vec<Graded> = Vec::new();
    let mut elapsed = Vec::new();
    let mut failed = 0;
    let mut answered_by = None;
    for (n, case) in cases.iter().enumerate() {
        let (questions, kinds) =
            questions_from_wire(&case.questions).map_err(|e| format!("{}: {e}", case.id))?;
        let started = Instant::now();
        let response = match backend.answer(&case.state, &model, &questions).await {
            Ok(r) => r,
            Err(e) => {
                failed += 1;
                eprintln!("{}: {e}", case.id);
                continue;
            }
        };
        elapsed.push(started.elapsed().as_secs_f64() * 1000.0);
        answered_by.get_or_insert_with(|| response.model.clone());
        for (id, kind) in &kinds {
            let Some(answer) = response.answers.get(id) else {
                return Err(format!("{}: no answer for question {id}", case.id));
            };
            let expected = case.gold.get(id).map(|g| g.label.as_str());
            let judgment = grade(answer, expected);
            graded.push(Graded {
                workflow: case.workflow.clone(),
                kind,
                judgment,
            });
        }
        if !args.json {
            eprintln!(
                "{:>4}/{} {} {:.0} ms",
                n + 1,
                cases.len(),
                case.id,
                elapsed.last().copied().unwrap_or_default()
            );
        }
    }

    let mut by_type: BTreeMap<String, Vec<&Judgment>> = BTreeMap::new();
    let mut by_workflow: BTreeMap<String, Vec<&Judgment>> = BTreeMap::new();
    for g in &graded {
        by_type
            .entry(g.kind.to_owned())
            .or_default()
            .push(&g.judgment);
        by_workflow
            .entry(g.workflow.clone())
            .or_default()
            .push(&g.judgment);
    }
    Ok(Report {
        backend: label,
        model: answered_by,
        cases: cases.len(),
        failed,
        overall: row(graded.iter().map(|g| &g.judgment)),
        by_type: by_type.into_iter().map(|(k, v)| (k, row(v))).collect(),
        by_workflow: by_workflow.into_iter().map(|(k, v)| (k, row(v))).collect(),
        latency: Latency::of(&elapsed),
    })
}

#[tokio::main]
async fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(2);
        }
    };
    let json = args.json;
    match run(args).await {
        Ok(report) if json => {
            println!(
                "{}",
                serde_json::to_string_pretty(&report).unwrap_or_default()
            );
            ExitCode::SUCCESS
        }
        Ok(report) => {
            println!(
                "backend: {}; model: {}; cases: {} ({} failed)",
                report.backend,
                report.model.as_deref().unwrap_or("-"),
                report.cases,
                report.failed
            );
            let overall = BTreeMap::from([("all questions".to_owned(), report.overall)]);
            print_table("overall", &overall);
            print_table("by type", &report.by_type);
            print_table("by workflow", &report.by_workflow);
            println!(
                "\nlatency per case: p50 {} ms, p95 {} ms, mean {} ms",
                fmt(report.latency.p50_ms),
                fmt(report.latency.p95_ms),
                fmt(report.latency.mean_ms)
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}
