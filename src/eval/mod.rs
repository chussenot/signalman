//! The evaluation harness: replay labelled alerts through the triage
//! questions, grade every judgment and the resulting decision, and report
//! accuracy, calibration and latency.
//!
//! Two modes share one grader. `run` calls the model and can record every
//! raw response; `replay` re-grades recorded responses under the current
//! configuration without calling the model, which is how thresholds and
//! wording are tuned: the judgments do not change when the policy does
//! (decision 0002), so inference need not be repeated to see a different
//! decision.
//!
//! Cases are JSON Lines: one object per line with an `id`, an `alert` in the
//! shape `signalman triage` accepts, and an `expected` block whose fields
//! are all optional. Only labelled fields are graded.

pub mod metrics;

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::answer::Response;
use crate::triage::{
    Alert, Decision, Impact, NO_DUPLICATE, OwnerCandidates, Policy, Texts, TriageAnswers,
    TriageQuestions, decide,
};
use crate::{Client, Request};

/// The decision's kind, as graded. The vocabulary belongs to the outcome
/// contract, which publishes it; the harness grades against the same values.
pub use crate::outcome::Action;

/// Bins for expected calibration error.
pub const ECE_BINS: usize = 10;

/// One labelled alert.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Case {
    /// Stable identifier; also the recording file name.
    pub id: String,
    /// The alert, as `signalman triage` reads it.
    pub alert: Alert,
    /// What a responder said the right answers were.
    #[serde(default)]
    pub expected: Expected,
}

/// Ground truth for a case. Every field optional; unlabelled judgments are
/// reported but not graded.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Expected {
    /// Owner candidate key.
    pub owner: Option<String>,
    /// Impact level.
    pub impact: Option<Impact>,
    /// Whether a person had to act.
    pub actionable: Option<bool>,
    /// Open incident id, or `none`.
    pub duplicate_of: Option<String>,
    /// Whether a listed change was the cause.
    pub caused_by_change: Option<bool>,
    /// The decision the policy should have reached.
    pub action: Option<Action>,
}

/// A raw model response kept for replay.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Recording {
    /// The case it answers.
    pub case: String,
    /// The response as received.
    pub response: Response,
    /// Wall-clock time of the call.
    pub elapsed_ms: u64,
}

/// One graded judgment.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Judgment {
    /// What the model chose (or the policy's reading of it).
    pub predicted: String,
    /// The label, when given.
    pub expected: Option<String>,
    /// `predicted == expected`, when labelled.
    pub correct: Option<bool>,
    /// The model's confidence in `predicted`: the Choice or Score
    /// confidence, or `max(p, 1 - p)` for a Noul.
    pub confidence: f64,
    /// Probability the model put on the expected option, when labelled.
    pub p_expected: Option<f64>,
    /// The full distribution, keyed by option.
    pub probabilities: BTreeMap<String, f64>,
    /// False when the question was not asked for this state (no open
    /// incidents, no recent changes) and the answer is the implied one.
    pub asked: bool,
}

/// One graded case.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Graded {
    /// Case id.
    pub id: String,
    /// Versioned model that answered.
    pub model: String,
    /// The decision the policy reached.
    pub decision: Decision,
    /// Its kind.
    pub action: Action,
    /// The labelled kind, when given.
    pub expected_action: Option<Action>,
    /// Judgments by question id.
    pub judgments: BTreeMap<String, Judgment>,
    /// Wall-clock time of the model call.
    pub elapsed_ms: u64,
    /// Tokens sent.
    pub input_tokens: u64,
    /// Tokens received.
    pub output_tokens: u64,
}

/// Metrics for one question over the labelled cases.
#[derive(Debug, Clone, PartialEq, Serialize, Default)]
pub struct QuestionMetrics {
    /// Cases with a label for this question.
    pub labelled: usize,
    /// Correct predictions.
    pub correct: usize,
    /// `correct / labelled`.
    pub accuracy: Option<f64>,
    /// Mean multi-class Brier score (0 perfect, 2 worst).
    pub brier: Option<f64>,
    /// Expected calibration error of the prediction confidence.
    pub ece: Option<f64>,
    /// Mean confidence when right.
    pub confidence_when_right: Option<f64>,
    /// Mean confidence when wrong.
    pub confidence_when_wrong: Option<f64>,
    /// `expected -> predicted -> count`.
    pub confusion: BTreeMap<String, BTreeMap<String, usize>>,
}

/// Metrics for the decision.
#[derive(Debug, Clone, PartialEq, Serialize, Default)]
pub struct DecisionMetrics {
    /// Cases with an expected action.
    pub labelled: usize,
    /// Decisions matching the label.
    pub correct: usize,
    /// `correct / labelled`.
    pub accuracy: Option<f64>,
    /// `expected -> reached -> count`.
    pub confusion: BTreeMap<String, BTreeMap<String, usize>>,
}

/// Latency summary in milliseconds.
#[derive(Debug, Clone, PartialEq, Serialize, Default)]
pub struct Latency {
    /// Median.
    pub p50_ms: Option<f64>,
    /// 95th percentile.
    pub p95_ms: Option<f64>,
    /// Mean.
    pub mean_ms: Option<f64>,
}

/// The report.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Report {
    /// Cases graded.
    pub cases: usize,
    /// Distinct versioned models seen (one, unless recordings mix).
    pub models: BTreeSet<String>,
    /// Per-question metrics, keyed by question id.
    pub questions: BTreeMap<String, QuestionMetrics>,
    /// Decision metrics.
    pub decision: DecisionMetrics,
    /// Latency.
    pub latency: Latency,
    /// Total input tokens.
    pub input_tokens: u64,
    /// Total output tokens.
    pub output_tokens: u64,
    /// Every graded case, for drill-down.
    pub graded: Vec<Graded>,
}

/// Harness failure.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Reading cases or recordings.
    #[error("cannot read {path}: {source}")]
    Io {
        /// The path.
        path: String,
        /// Cause.
        #[source]
        source: std::io::Error,
    },
    /// A case line or recording that does not parse.
    #[error("invalid JSON in {context}: {source}")]
    Json {
        /// File and line.
        context: String,
        /// Cause.
        #[source]
        source: serde_json::Error,
    },
    /// Two cases share an id.
    #[error("duplicate case id {0}")]
    DuplicateId(String),
    /// Replay found no recording for a case.
    #[error("no recording for case {0} (expected {1})")]
    MissingRecording(String, String),
    /// Building questions or reading answers.
    #[error(transparent)]
    TypeSafe(#[from] crate::Error),
}

/// Result alias.
pub type Result<T> = std::result::Result<T, Error>;

/// Parse JSON Lines. Blank lines and lines starting with `#` are skipped.
pub fn parse_cases(text: &str, path: &str) -> Result<Vec<Case>> {
    let mut cases = Vec::new();
    let mut seen = BTreeSet::new();
    for (i, line) in text.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let case: Case = serde_json::from_str(trimmed).map_err(|source| Error::Json {
            context: format!("{path}:{}", i + 1),
            source,
        })?;
        if !seen.insert(case.id.clone()) {
            return Err(Error::DuplicateId(case.id));
        }
        cases.push(case);
    }
    Ok(cases)
}

/// Read and parse a JSON Lines file.
pub fn read_cases(path: &Path) -> Result<Vec<Case>> {
    let text = std::fs::read_to_string(path).map_err(|source| Error::Io {
        path: path.display().to_string(),
        source,
    })?;
    parse_cases(&text, &path.display().to_string())
}

/// What the harness needs from the configuration.
#[derive(Debug, Clone)]
pub struct Setup<'a> {
    /// Question wording.
    pub texts: &'a Texts,
    /// Owner candidates for every case (no catalog in the harness).
    pub candidates: &'a OwnerCandidates,
    /// Routing thresholds.
    pub policy: &'a Policy,
}

/// Call the model for every case, grade, and optionally record each raw
/// response as `<record>/<id>.json`.
pub async fn run(
    client: &Client,
    model: &str,
    cases: &[Case],
    setup: &Setup<'_>,
    record: Option<&Path>,
) -> Result<Report> {
    if let Some(dir) = record {
        std::fs::create_dir_all(dir).map_err(|source| Error::Io {
            path: dir.display().to_string(),
            source,
        })?;
    }
    let mut graded = Vec::with_capacity(cases.len());
    for case in cases {
        let questions = TriageQuestions::for_alert_with_texts(
            &case.alert,
            setup.candidates.clone(),
            setup.texts,
        )?;
        let state = TriageQuestions::state(&case.alert);
        let request = Request {
            state: &state,
            model,
            questions: &questions.questions,
        };
        let started = Instant::now();
        let response = client.evaluate(&request).await?;
        let elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        if let Some(dir) = record {
            let path = dir.join(format!("{}.json", case.id));
            let rec = Recording {
                case: case.id.clone(),
                response: response.clone(),
                elapsed_ms,
            };
            let text = serde_json::to_string_pretty(&rec).map_err(|source| Error::Json {
                context: path.display().to_string(),
                source,
            })?;
            // Committed recordings end with a newline like any text file.
            std::fs::write(&path, text + "\n").map_err(|source| Error::Io {
                path: path.display().to_string(),
                source,
            })?;
        }
        graded.push(grade(
            case,
            &questions,
            &response,
            setup.policy,
            elapsed_ms,
        )?);
    }
    Ok(report(graded))
}

/// Grade recorded responses under the current setup without calling the
/// model. Every case needs `<dir>/<id>.json`.
pub fn replay(dir: &Path, cases: &[Case], setup: &Setup<'_>) -> Result<Report> {
    let mut graded = Vec::with_capacity(cases.len());
    for case in cases {
        let path = dir.join(format!("{}.json", case.id));
        let text = std::fs::read_to_string(&path)
            .map_err(|_| Error::MissingRecording(case.id.clone(), path.display().to_string()))?;
        let rec: Recording = serde_json::from_str(&text).map_err(|source| Error::Json {
            context: path.display().to_string(),
            source,
        })?;
        let questions = TriageQuestions::for_alert_with_texts(
            &case.alert,
            setup.candidates.clone(),
            setup.texts,
        )?;
        graded.push(grade(
            case,
            &questions,
            &rec.response,
            setup.policy,
            rec.elapsed_ms,
        )?);
    }
    Ok(report(graded))
}

/// Read the answers through the handles, decide, and grade against the labels.
pub fn grade(
    case: &Case,
    questions: &TriageQuestions,
    response: &Response,
    policy: &Policy,
    elapsed_ms: u64,
) -> Result<Graded> {
    let answers = questions.read(response)?;
    let decision = decide(&answers, policy);
    Ok(Graded {
        id: case.id.clone(),
        model: response.model.clone(),
        action: Action::from(&decision),
        expected_action: case.expected.action,
        judgments: judgments(&case.expected, &answers),
        decision,
        elapsed_ms,
        input_tokens: response.usage.input_tokens,
        output_tokens: response.usage.output_tokens,
    })
}

fn judgments(expected: &Expected, a: &TriageAnswers) -> BTreeMap<String, Judgment> {
    let mut out = BTreeMap::new();

    let owner_probs: BTreeMap<String, f64> = a
        .owner
        .probabilities
        .iter()
        .map(|(k, p)| (k.clone(), p.value()))
        .collect();
    out.insert(
        "owner".into(),
        judge(
            a.owner.chosen.clone(),
            expected.owner.clone(),
            a.owner.confidence.value(),
            owner_probs,
            true,
        ),
    );

    let impact_probs: BTreeMap<String, f64> = a
        .impact
        .probabilities
        .iter()
        .enumerate()
        .map(|(i, p)| (Impact::from_level(i).key().to_owned(), p.value()))
        .collect();
    out.insert(
        "impact".into(),
        judge(
            Impact::from_level(a.impact.nearest_level())
                .key()
                .to_owned(),
            expected.impact.map(|i| i.key().to_owned()),
            a.impact.confidence.value(),
            impact_probs,
            true,
        ),
    );

    out.insert(
        "actionable".into(),
        judge_noul(a.actionable.yes.value(), expected.actionable, true),
    );

    out.insert(
        "duplicate_of".into(),
        match &a.duplicate_of {
            Some(dup) => judge(
                dup.chosen.clone(),
                expected.duplicate_of.clone(),
                dup.confidence.value(),
                dup.probabilities
                    .iter()
                    .map(|(k, p)| (k.clone(), p.value()))
                    .collect(),
                true,
            ),
            // Not asked: there were no open incidents, so the implied answer
            // is `none` with certainty.
            None => judge(
                NO_DUPLICATE.to_owned(),
                expected.duplicate_of.clone(),
                1.0,
                BTreeMap::from([(NO_DUPLICATE.to_owned(), 1.0)]),
                false,
            ),
        },
    );

    out.insert(
        "caused_by_change".into(),
        match a.caused_by_change {
            Some(n) => judge_noul(n.yes.value(), expected.caused_by_change, true),
            // Not asked: no recent changes were listed, so the implied
            // answer is no.
            None => judge_noul(0.0, expected.caused_by_change, false),
        },
    );

    out
}

fn judge(
    predicted: String,
    expected: Option<String>,
    confidence: f64,
    probabilities: BTreeMap<String, f64>,
    asked: bool,
) -> Judgment {
    let correct = expected.as_ref().map(|e| *e == predicted);
    let p_expected = expected
        .as_ref()
        .map(|e| probabilities.get(e).copied().unwrap_or(0.0));
    Judgment {
        predicted,
        expected,
        correct,
        confidence,
        p_expected,
        probabilities,
        asked,
    }
}

fn judge_noul(p_yes: f64, expected: Option<bool>, asked: bool) -> Judgment {
    let predicted = if p_yes >= 0.5 { "yes" } else { "no" };
    judge(
        predicted.to_owned(),
        expected.map(|b| if b { "yes".to_owned() } else { "no".to_owned() }),
        p_yes.max(1.0 - p_yes),
        BTreeMap::from([("yes".to_owned(), p_yes), ("no".to_owned(), 1.0 - p_yes)]),
        asked,
    )
}

/// Aggregate graded cases.
#[allow(clippy::cast_precision_loss)] // counts and milliseconds, far below 2^52
pub fn report(graded: Vec<Graded>) -> Report {
    let mut questions: BTreeMap<String, QuestionMetrics> = BTreeMap::new();
    let mut brier: BTreeMap<String, Vec<f64>> = BTreeMap::new();
    let mut calib: BTreeMap<String, Vec<(f64, bool)>> = BTreeMap::new();
    let mut right: BTreeMap<String, Vec<f64>> = BTreeMap::new();
    let mut wrong: BTreeMap<String, Vec<f64>> = BTreeMap::new();
    let mut decision = DecisionMetrics::default();
    let mut latencies = Vec::with_capacity(graded.len());
    let mut models = BTreeSet::new();
    let (mut input_tokens, mut output_tokens) = (0u64, 0u64);

    for g in &graded {
        models.insert(g.model.clone());
        latencies.push(g.elapsed_ms as f64);
        input_tokens += g.input_tokens;
        output_tokens += g.output_tokens;
        for (qid, j) in &g.judgments {
            let m = questions.entry(qid.clone()).or_default();
            let (Some(expected), Some(correct)) = (&j.expected, j.correct) else {
                continue;
            };
            m.labelled += 1;
            if correct {
                m.correct += 1;
                right.entry(qid.clone()).or_default().push(j.confidence);
            } else {
                wrong.entry(qid.clone()).or_default().push(j.confidence);
            }
            *m.confusion
                .entry(expected.clone())
                .or_default()
                .entry(j.predicted.clone())
                .or_default() += 1;
            let pairs: Vec<(bool, f64)> = j
                .probabilities
                .iter()
                .map(|(k, p)| (k == expected, *p))
                .collect();
            // An expected option the model never offered counts as a miss
            // with probability zero.
            let mut b = metrics::brier(&pairs);
            if !j.probabilities.contains_key(expected) {
                b += 1.0;
            }
            brier.entry(qid.clone()).or_default().push(b);
            calib
                .entry(qid.clone())
                .or_default()
                .push((j.confidence, correct));
        }
        if let Some(exp) = g.expected_action {
            decision.labelled += 1;
            if exp == g.action {
                decision.correct += 1;
            }
            *decision
                .confusion
                .entry(exp.key().to_owned())
                .or_default()
                .entry(g.action.key().to_owned())
                .or_default() += 1;
        }
    }
    for (qid, m) in &mut questions {
        if m.labelled > 0 {
            m.accuracy = Some(m.correct as f64 / m.labelled as f64);
        }
        m.brier = brier.get(qid).and_then(|v| metrics::mean(v));
        m.ece = calib
            .get(qid)
            .filter(|v| !v.is_empty())
            .map(|v| metrics::expected_calibration_error(v, ECE_BINS));
        m.confidence_when_right = right.get(qid).and_then(|v| metrics::mean(v));
        m.confidence_when_wrong = wrong.get(qid).and_then(|v| metrics::mean(v));
    }
    if decision.labelled > 0 {
        decision.accuracy = Some(decision.correct as f64 / decision.labelled as f64);
    }
    Report {
        cases: graded.len(),
        models,
        questions,
        decision,
        latency: Latency {
            p50_ms: metrics::percentile(&latencies, 0.5),
            p95_ms: metrics::percentile(&latencies, 0.95),
            mean_ms: metrics::mean(&latencies),
        },
        input_tokens,
        output_tokens,
        graded,
    }
}

impl Report {
    /// A plain-text rendering for the terminal.
    #[allow(clippy::too_many_lines)] // one table after another, read top to bottom
    pub fn render(&self) -> String {
        use std::fmt::Write;
        let mut s = String::new();
        let fmt = |v: Option<f64>| v.map_or("   -".to_owned(), |x| format!("{x:.2}"));
        let _ = writeln!(
            s,
            "cases     {}   model {}",
            self.cases,
            self.models.iter().cloned().collect::<Vec<_>>().join(", ")
        );
        let _ = writeln!(
            s,
            "decision  labelled {}  agreement {}",
            self.decision.labelled,
            fmt(self.decision.accuracy)
        );
        let _ = writeln!(s);
        let _ = writeln!(
            s,
            "{:<18} {:>3} {:>5} {:>6} {:>5} {:>10} {:>10}",
            "question", "n", "acc", "brier", "ece", "conf|right", "conf|wrong"
        );
        for qid in [
            "owner",
            "impact",
            "actionable",
            "duplicate_of",
            "caused_by_change",
        ] {
            let Some(m) = self.questions.get(qid) else {
                continue;
            };
            let _ = writeln!(
                s,
                "{:<18} {:>3} {:>5} {:>6} {:>5} {:>10} {:>10}",
                qid,
                m.labelled,
                fmt(m.accuracy),
                fmt(m.brier),
                fmt(m.ece),
                fmt(m.confidence_when_right),
                fmt(m.confidence_when_wrong)
            );
        }
        let _ = writeln!(s);
        let _ = writeln!(
            s,
            "latency   p50 {} ms  p95 {} ms  mean {} ms   tokens in {} out {}",
            self.latency
                .p50_ms
                .map_or("-".into(), |v| format!("{v:.0}")),
            self.latency
                .p95_ms
                .map_or("-".into(), |v| format!("{v:.0}")),
            self.latency
                .mean_ms
                .map_or("-".into(), |v| format!("{v:.0}")),
            self.input_tokens,
            self.output_tokens
        );
        let mut mismatches = Vec::new();
        for g in &self.graded {
            for (qid, j) in &g.judgments {
                if j.correct == Some(false) {
                    mismatches.push(format!(
                        "  {:<14} {:<16} expected {}, got {} (p {:.2}, confidence {:.2}{})",
                        g.id,
                        qid,
                        j.expected.as_deref().unwrap_or("-"),
                        j.predicted,
                        j.p_expected.unwrap_or(0.0),
                        j.confidence,
                        if j.asked { "" } else { ", not asked" }
                    ));
                }
            }
            if let Some(exp) = g.expected_action
                && exp != g.action
            {
                mismatches.push(format!(
                    "  {:<14} {:<16} expected {}, got {}",
                    g.id,
                    "decision",
                    exp.key(),
                    g.action.key()
                ));
            }
        }
        if mismatches.is_empty() {
            let _ = writeln!(s, "\nno mismatches against the labels");
        } else {
            let _ = writeln!(s, "\nmismatches ({}):", mismatches.len());
            for m in mismatches {
                let _ = writeln!(s, "{m}");
            }
        }
        s
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn cases_parse_from_json_lines_with_comments_and_reject_duplicates() {
        let text = r#"
# a comment
{"id": "a", "alert": {"source": "p", "title": "T", "description": "d"}, "expected": {"owner": "platform", "actionable": true}}

{"id": "b", "alert": {"source": "p", "title": "U", "description": "e"}}
"#;
        let cases = parse_cases(text, "t.jsonl").unwrap();
        assert_eq!(cases.len(), 2);
        assert_eq!(cases[0].expected.owner.as_deref(), Some("platform"));
        assert_eq!(cases[1].expected, Expected::default());

        let dup = format!(
            "{}\n{}",
            text.lines().nth(2).unwrap(),
            text.lines().nth(2).unwrap()
        );
        assert!(matches!(
            parse_cases(&dup, "t.jsonl"),
            Err(Error::DuplicateId(id)) if id == "a"
        ));
        assert!(parse_cases(r#"{"id": "x", "alert": {}, "expected": {"sev": 1}}"#, "t").is_err());
    }

    #[test]
    fn action_round_trips_its_snake_case_name() {
        for a in [
            Action::Suppress,
            Action::AttachToIncident,
            Action::Page,
            Action::Ticket,
            Action::HumanTriage,
        ] {
            let json = serde_json::to_string(&a).unwrap();
            assert_eq!(json, format!("\"{}\"", a.key()));
            assert_eq!(serde_json::from_str::<Action>(&json).unwrap(), a);
        }
    }
}
