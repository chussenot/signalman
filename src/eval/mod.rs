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
//! A live run keeps going past a case whose answer does not fit its
//! questions (an option the question never offered, a Score off its scale):
//! the client refuses such a response, and one bad answer should not throw
//! away the rest of a paid run. The case is listed in [`Report::failed`]
//! with the error and its request id, is not graded and is not recorded, so
//! the accuracy is over the graded cases. Any other error (the network, a
//! key, a case that does not build) stops the run. A replay stays strict: a
//! recording that no longer fits means the setup changed under it, and
//! grading around it would hide that.
//!
//! Cases are JSON Lines: one object per line with an `id`, an `alert` in the
//! shape `signalman triage` accepts, and an `expected` block whose fields
//! are all optional. Only labelled fields are graded.
//!
//! The measuring is [`judgment::eval`]'s: recordings, one [`Judgment`] per
//! answer and label, and [`QuestionMetrics`] per question. What is
//! signalman's is the mapping from the triage answers to labels, and the
//! decision the policy reaches from them.

/// Scoring rules, re-exported from the judgment crate.
pub use judgment::eval::metrics;
/// The generic measuring types, re-exported from the judgment crate.
pub use judgment::eval::{ECE_BINS, Judgment, Latency, QuestionMetrics, Recording};

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::Instant;

use judgment::SystemOne;
use serde::{Deserialize, Serialize};

use crate::answer::Response;
use crate::triage::{
    Alert, Decision, Impact, NO_DUPLICATE, OwnerCandidates, Policy, Texts, TriageAnswers,
    TriageQuestions, decide,
};

/// The decision's kind, as graded. The vocabulary belongs to the outcome
/// contract, which publishes it; the harness grades against the same values.
pub use crate::outcome::Action;

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

/// A case a live run could not grade: the model's answer did not fit the
/// questions it was sent ([`judgment::Error::is_unfit`]), so the client
/// refused it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FailedCase {
    /// Case id.
    pub id: String,
    /// What did not fit, naming the question.
    pub error: String,
    /// TypeSafe's request id for the call, to report the answer with.
    pub request_id: Option<String>,
}

/// The report.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Report {
    /// Cases graded. A failed case is not counted here.
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
    /// Cases a live run could not grade because the answer did not fit the
    /// questions; left out of every metric above. Omitted from the JSON when
    /// empty, so a report where every case was graded reads as before.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub failed: Vec<FailedCase>,
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
    /// Reading or writing a recording.
    #[error(transparent)]
    Recording(judgment::eval::Error),
    /// Building questions or reading answers.
    #[error(transparent)]
    TypeSafe(#[from] crate::Error),
}

impl From<judgment::eval::Error> for Error {
    fn from(e: judgment::eval::Error) -> Self {
        match e {
            judgment::eval::Error::MissingRecording { case, path } => {
                Self::MissingRecording(case, path)
            }
            other => Self::Recording(other),
        }
    }
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
///
/// A case whose answer does not fit its questions is listed in
/// [`Report::failed`] and the run continues; it is neither graded nor
/// recorded. Any other error stops the run.
pub async fn run(
    backend: &dyn SystemOne,
    model: &str,
    cases: &[Case],
    setup: &Setup<'_>,
    record: Option<&Path>,
) -> Result<Report> {
    let mut graded = Vec::with_capacity(cases.len());
    let mut failed = Vec::new();
    for case in cases {
        let questions = TriageQuestions::for_alert_with_texts(
            &case.alert,
            setup.candidates.clone(),
            setup.texts,
        )?;
        let state = TriageQuestions::state(&case.alert);
        let started = Instant::now();
        let response = match backend.answer(&state, model, &questions.questions).await {
            Ok(r) => r,
            Err(e) if e.is_unfit() => {
                failed.push(FailedCase {
                    id: case.id.clone(),
                    error: e.to_string(),
                    request_id: e.request_id().map(str::to_owned),
                });
                continue;
            }
            Err(e) => return Err(e.into()),
        };
        let elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        if let Some(dir) = record {
            judgment::eval::write_recording(
                dir,
                &Recording {
                    case: case.id.clone(),
                    response: response.clone(),
                    elapsed_ms,
                    request_hash: None,
                },
            )?;
        }
        graded.push(grade(
            case,
            &questions,
            &response,
            setup.policy,
            elapsed_ms,
        )?);
    }
    Ok(report(graded, failed))
}

/// Grade recorded responses under the current setup without calling the
/// model. Every case needs `<dir>/<id>.json`.
pub fn replay(dir: &Path, cases: &[Case], setup: &Setup<'_>) -> Result<Report> {
    let mut graded = Vec::with_capacity(cases.len());
    for case in cases {
        let rec = judgment::eval::read_recording(dir, &case.id)?;
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
    Ok(report(graded, Vec::new()))
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
        Judgment::new(
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
        Judgment::new(
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
        Judgment::noul(a.actionable.yes.value(), expected.actionable, true),
    );

    out.insert(
        "duplicate_of".into(),
        match &a.duplicate_of {
            Some(dup) => Judgment::new(
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
            None => Judgment::new(
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
            Some(n) => Judgment::noul(n.yes.value(), expected.caused_by_change, true),
            // Not asked: no recent changes were listed, so the implied
            // answer is no.
            None => Judgment::noul(0.0, expected.caused_by_change, false),
        },
    );

    out
}

/// Aggregate graded cases, beside the cases that could not be graded.
#[allow(clippy::cast_precision_loss)] // counts and milliseconds, far below 2^52
pub fn report(graded: Vec<Graded>, failed: Vec<FailedCase>) -> Report {
    let mut by_question: BTreeMap<String, Vec<&Judgment>> = BTreeMap::new();
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
            by_question.entry(qid.clone()).or_default().push(j);
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
    let questions = by_question
        .into_iter()
        .map(|(qid, js)| (qid, QuestionMetrics::summarise(js, ECE_BINS)))
        .collect();
    if decision.labelled > 0 {
        decision.accuracy = Some(decision.correct as f64 / decision.labelled as f64);
    }
    Report {
        cases: graded.len(),
        models,
        questions,
        decision,
        latency: Latency::of(&latencies),
        input_tokens,
        output_tokens,
        graded,
        failed,
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
        if !self.failed.is_empty() {
            let _ = writeln!(
                s,
                "\nfailed ({}), not graded: the answer did not fit the questions, so the \
                 figures above are over the {} graded cases:",
                self.failed.len(),
                self.cases
            );
            // The error ends with ` [request_id …]` when the call had one.
            for f in &self.failed {
                let _ = writeln!(s, "  {:<14} {}", f.id, f.error);
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
