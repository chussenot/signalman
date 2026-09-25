//! Measuring judgments: recordings for replay, one graded judgment at a
//! time, and the metrics that say whether a model is right and whether its
//! confidence means anything.
//!
//! A decision model is only useful if its probabilities can be trusted, and
//! that is a property to measure, not assume: a model that is right 80% of
//! the time at 0.8 confidence is calibrated, one that is right 50% of the
//! time at 0.95 is not, and the thresholds an application sets on those
//! numbers are only as good as that. This module keeps the measuring
//! generic. What a label means, which question maps to which label, and
//! what decision the answers should have produced belong to the
//! application; it hands each answer and its label to [`Judgment`] and each
//! question's judgments to [`QuestionMetrics::summarise`].
//!
//! Recording keeps inference and tuning apart: the judgments do not change
//! when a threshold does, so a response recorded once is graded again under
//! every candidate policy without a model call. [`Recording`] is the file
//! format, keyed by a case id for a harness or by a request hash for a
//! [`crate::backend::Replay`].

pub mod metrics;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::answer::{Answer, Response, sanitize_kind};
use crate::question::Questions;

/// Bins for expected calibration error.
pub const ECE_BINS: usize = 10;

/// A raw model response kept for replay.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Recording {
    /// The case it answers, or the request hash when recorded by a
    /// [`crate::backend::Recorder`].
    pub case: String,
    /// The response as received.
    pub response: Response,
    /// Wall-clock time of the call.
    pub elapsed_ms: u64,
    /// Content hash of the request, when recorded by a
    /// [`crate::backend::Recorder`]; absent in a harness's recordings, which
    /// are keyed by case.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_hash: Option<String>,
}

/// Reading or writing recordings.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A file could not be read or written.
    #[error("cannot access {path}: {source}")]
    Io {
        /// The path.
        path: String,
        /// Cause.
        #[source]
        source: std::io::Error,
    },
    /// A recording that does not parse.
    #[error("invalid recording {path}: {source}")]
    Json {
        /// The path.
        path: String,
        /// Cause.
        #[source]
        source: serde_json::Error,
    },
    /// No recording for the case.
    #[error("no recording for {case} (expected {path})")]
    MissingRecording {
        /// The case id.
        case: String,
        /// Where it was expected.
        path: String,
    },
}

/// Result alias.
pub type Result<T> = std::result::Result<T, Error>;

/// Where a recording lives: `dir/<case>.json`.
pub fn recording_path(dir: &Path, case: &str) -> PathBuf {
    dir.join(format!("{case}.json"))
}

/// Write `recording` under `dir` (created if needed), pretty-printed and
/// newline-terminated like any committed text file.
pub fn write_recording(dir: &Path, recording: &Recording) -> Result<PathBuf> {
    std::fs::create_dir_all(dir).map_err(|source| Error::Io {
        path: dir.display().to_string(),
        source,
    })?;
    let path = recording_path(dir, &recording.case);
    let text = serde_json::to_string_pretty(recording).map_err(|source| Error::Json {
        path: path.display().to_string(),
        source,
    })?;
    std::fs::write(&path, text + "\n").map_err(|source| Error::Io {
        path: path.display().to_string(),
        source,
    })?;
    Ok(path)
}

/// Read the recording for `case` under `dir`.
pub fn read_recording(dir: &Path, case: &str) -> Result<Recording> {
    let path = recording_path(dir, case);
    let text = std::fs::read_to_string(&path).map_err(|_| Error::MissingRecording {
        case: case.to_owned(),
        path: path.display().to_string(),
    })?;
    serde_json::from_str(&text).map_err(|source| Error::Json {
        path: path.display().to_string(),
        source,
    })
}

/// A stable content hash of a request: the same state and questions give
/// the same hash whatever order their keys were inserted in and whatever
/// model alias was asked for.
///
/// It names recording files, so it must not change between runs, machines
/// or toolchains. The standard library's `DefaultHasher` promises none of
/// that (its algorithm and seeding may change between Rust versions), so
/// this is FNV-1a over a canonical rendering of the JSON with keys sorted at
/// every level: sixteen hex digits, no dependency, the same result anywhere.
/// FNV is not collision-resistant against an adversary; for a directory of
/// recordings an accidental collision is negligible.
pub fn request_hash(state: &Value, questions: &Questions) -> String {
    let questions = serde_json::to_value(questions).unwrap_or(Value::Null);
    let mut canonical = String::new();
    write_canonical(
        &mut canonical,
        &Value::Object(
            [
                ("questions".to_owned(), questions),
                ("state".to_owned(), state.clone()),
            ]
            .into_iter()
            .collect(),
        ),
    );
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in canonical.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// JSON with object keys sorted at every level, so two equal values render
/// the same bytes whichever order their keys were inserted in.
fn write_canonical(out: &mut String, value: &Value) {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (i, key) in keys.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&serde_json::to_string(key).unwrap_or_default());
                out.push(':');
                write_canonical(out, &map[*key]);
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_canonical(out, item);
            }
            out.push(']');
        }
        scalar => out.push_str(&serde_json::to_string(scalar).unwrap_or_default()),
    }
}

/// One graded judgment: what the model said, what the label said, and how
/// sure the model was.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Judgment {
    /// What the model chose (or the application's reading of it).
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
    /// False when the question was not asked for this state and the answer
    /// is the implied one; such judgments are graded but marked.
    pub asked: bool,
}

impl Judgment {
    /// Grade a prediction against a label over its distribution.
    pub fn new(
        predicted: String,
        expected: Option<String>,
        confidence: f64,
        probabilities: BTreeMap<String, f64>,
        asked: bool,
    ) -> Self {
        let correct = expected.as_ref().map(|e| *e == predicted);
        let p_expected = expected
            .as_ref()
            .map(|e| probabilities.get(e).copied().unwrap_or(0.0));
        Self {
            predicted,
            expected,
            correct,
            confidence,
            p_expected,
            probabilities,
            asked,
        }
    }

    /// Grade a yes/no probability against a boolean label: `yes` at 0.5 and
    /// above, confidence `max(p, 1 - p)`.
    pub fn noul(p_yes: f64, expected: Option<bool>, asked: bool) -> Self {
        let predicted = if p_yes >= 0.5 { "yes" } else { "no" };
        Self::new(
            predicted.to_owned(),
            expected.map(|b| if b { "yes".to_owned() } else { "no".to_owned() }),
            p_yes.max(1.0 - p_yes),
            BTreeMap::from([("yes".to_owned(), p_yes), ("no".to_owned(), 1.0 - p_yes)]),
            asked,
        )
    }

    /// Grade a wire answer as the model returned it: a Choice by its option
    /// keys, a Score by its level indices as strings, a Noul as `yes` or
    /// `no`. An application that names its levels grades through
    /// [`Self::new`] instead.
    ///
    /// An [`Answer::Unknown`] is graded, not dropped, because dropping it
    /// would raise the accuracy of a model whose answers could not be read.
    /// It is a miss whenever there is a label, even an empty one: predicted
    /// `<kind>` (the escaped kind in angle brackets, which no option key
    /// equals), probability zero on the label, confidence 0.0 and no
    /// distribution. In [`QuestionMetrics::summarise`] it lowers accuracy
    /// and costs a Brier score of 1.0, but its (0.0, wrong) calibration pair
    /// is perfectly calibrated, so it pulls the expected calibration error
    /// and the confidence when wrong toward zero. That is accepted because
    /// the case is rare and loud: an unknown kind means this crate is older
    /// than the server, the client logs each one at `warn`, and the
    /// `<kind>` it predicts stands out in the confusion matrix. It is rarer
    /// still because every backend verifies its response
    /// ([`Response::verify`](crate::Response::verify)), which refuses an
    /// unknown answer under an asked question: only an unverified response,
    /// such as one read by case id with [`read_recording`] or built by hand,
    /// or an answer to a question nobody asked, can bring one here.
    pub fn of_answer(answer: &Answer, expected: Option<&str>) -> Self {
        match answer {
            Answer::Unknown(_) => Self {
                predicted: format!("<{}>", sanitize_kind(answer.kind())),
                expected: expected.map(str::to_owned),
                correct: expected.map(|_| false),
                confidence: 0.0,
                p_expected: expected.map(|_| 0.0),
                probabilities: BTreeMap::new(),
                asked: true,
            },
            Answer::Noul { noul } => Self::noul(noul.value(), expected.map(|e| e == "yes"), true),
            Answer::Choice {
                choice,
                probabilities,
                confidence,
            } => Self::new(
                choice.clone(),
                expected.map(str::to_owned),
                confidence.value(),
                probabilities
                    .iter()
                    .map(|(k, p)| (k.clone(), p.value()))
                    .collect(),
                true,
            ),
            Answer::Score {
                probabilities,
                confidence,
                ..
            } => {
                let probs: BTreeMap<String, f64> = probabilities
                    .iter()
                    .map(|(k, p)| (k.clone(), p.value()))
                    .collect();
                let predicted = probs
                    .iter()
                    .max_by(|a, b| a.1.partial_cmp(b.1).unwrap_or(std::cmp::Ordering::Equal))
                    .map(|(k, _)| k.clone())
                    .unwrap_or_default();
                Self::new(
                    predicted,
                    expected.map(str::to_owned),
                    confidence.value(),
                    probs,
                    true,
                )
            }
        }
    }
}

/// Metrics for one question over its labelled judgments.
#[derive(Debug, Clone, PartialEq, Serialize, Default)]
pub struct QuestionMetrics {
    /// Judgments with a label.
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

impl QuestionMetrics {
    /// Aggregate the judgments of one question; unlabelled ones are
    /// counted in nothing. An expected option the model never offered counts
    /// as a miss with probability zero, so a label outside the option set
    /// hurts the score rather than vanishing. `ece_bins` is usually
    /// [`ECE_BINS`].
    #[allow(clippy::cast_precision_loss)] // counts, far below 2^52
    pub fn summarise<'a>(
        judgments: impl IntoIterator<Item = &'a Judgment>,
        ece_bins: usize,
    ) -> Self {
        let mut m = Self::default();
        let mut brier = Vec::new();
        let mut calib = Vec::new();
        let mut right = Vec::new();
        let mut wrong = Vec::new();
        for j in judgments {
            let (Some(expected), Some(correct)) = (&j.expected, j.correct) else {
                continue;
            };
            m.labelled += 1;
            if correct {
                m.correct += 1;
                right.push(j.confidence);
            } else {
                wrong.push(j.confidence);
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
            let mut b = metrics::brier(&pairs);
            if !j.probabilities.contains_key(expected) {
                b += 1.0;
            }
            brier.push(b);
            calib.push((j.confidence, correct));
        }
        if m.labelled > 0 {
            m.accuracy = Some(m.correct as f64 / m.labelled as f64);
        }
        m.brier = metrics::mean(&brier);
        m.ece = (!calib.is_empty()).then(|| metrics::expected_calibration_error(&calib, ece_bins));
        m.confidence_when_right = metrics::mean(&right);
        m.confidence_when_wrong = metrics::mean(&wrong);
        m
    }
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

impl Latency {
    /// Summarise call times in milliseconds.
    pub fn of(elapsed_ms: &[f64]) -> Self {
        Self {
            p50_ms: metrics::percentile(elapsed_ms, 0.5),
            p95_ms: metrics::percentile(elapsed_ms, 0.95),
            mean_ms: metrics::mean(elapsed_ms),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::answer::{Confidence, Probability, Usage};
    use serde_json::json;

    #[test]
    fn request_hash_ignores_key_order_and_sees_content() {
        let mut q = Questions::new();
        q.noul("a", "Is `x` set?", None).unwrap();
        let one = json!({ "x": 1, "y": { "b": 2, "a": 1 } });
        let two = json!({ "y": { "a": 1, "b": 2 }, "x": 1 });
        assert_eq!(request_hash(&one, &q), request_hash(&two, &q));
        assert_ne!(request_hash(&json!({ "x": 2 }), &q), request_hash(&one, &q));
        let mut q2 = Questions::new();
        q2.noul("a", "Is `x` unset?", None).unwrap();
        assert_ne!(request_hash(&one, &q2), request_hash(&one, &q));
        assert_eq!(request_hash(&one, &q).len(), 16);
    }

    #[test]
    fn recordings_round_trip_with_and_without_a_hash() {
        let dir = std::env::temp_dir().join(format!("judgment-rec-{}", std::process::id()));
        let response = Response {
            model: "m".into(),
            answers: BTreeMap::from([
                (
                    "a".to_owned(),
                    Answer::Noul {
                        noul: Probability::new(0.7).unwrap(),
                    },
                ),
                // An answer of a kind this release does not know is kept.
                (
                    "b".to_owned(),
                    Answer::Unknown(json!({ "type": "rank", "ranking": ["x", "y"] })),
                ),
            ]),
            usage: Usage {
                input_tokens: 1,
                output_tokens: 2,
            },
            request_id: None,
            // And so is a field the server added beyond the documented shape.
            extra: BTreeMap::from([("routing".to_owned(), json!({ "model": "typed-decisions" }))]),
        };
        let keyed = Recording {
            case: "case-1".into(),
            response: response.clone(),
            elapsed_ms: 5,
            request_hash: None,
        };
        write_recording(&dir, &keyed).unwrap();
        // The harness's format has no hash field at all.
        let text = std::fs::read_to_string(recording_path(&dir, "case-1")).unwrap();
        assert!(!text.contains("request_hash"));
        assert!(text.ends_with('\n'));
        assert!(text.contains(r#""routing": {"#), "{text}");
        assert!(text.contains(r#""type": "rank""#), "{text}");
        assert_eq!(read_recording(&dir, "case-1").unwrap(), keyed);
        assert!(matches!(
            read_recording(&dir, "ghost"),
            Err(Error::MissingRecording { case, .. }) if case == "ghost"
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_recording_made_before_0_2_reads_with_no_request_id() {
        // The shape every recording had before responses carried the
        // request id: no `request_id` key anywhere.
        let text = r#"{
          "case": "0123456789abcdef",
          "response": {
            "model": "typed-decisions",
            "answers": { "a": { "type": "noul", "noul": 0.25 } },
            "usage": { "input_tokens": 7, "output_tokens": 1 }
          },
          "elapsed_ms": 12,
          "request_hash": "0123456789abcdef"
        }"#;
        let recording: Recording = serde_json::from_str(text).unwrap();
        assert_eq!(recording.response.request_id, None);
        // Written back, it gains no key: an old recording re-serialises as
        // it was.
        let again = serde_json::to_string(&recording).unwrap();
        assert!(!again.contains("request_id"), "{again}");
    }

    #[test]
    fn judgments_grade_each_primitive_and_metrics_summarise_them() {
        let choice = Answer::Choice {
            choice: "billing".into(),
            probabilities: BTreeMap::from([
                ("billing".to_owned(), Probability::new(0.8).unwrap()),
                ("technical".to_owned(), Probability::new(0.2).unwrap()),
            ]),
            confidence: Confidence::new(0.7).unwrap(),
        };
        let right = Judgment::of_answer(&choice, Some("billing"));
        assert_eq!(right.correct, Some(true));
        assert_eq!(right.p_expected, Some(0.8));
        let wrong = Judgment::of_answer(&choice, Some("sales"));
        assert_eq!(wrong.correct, Some(false));
        assert_eq!(wrong.p_expected, Some(0.0));
        let unlabelled = Judgment::of_answer(&choice, None);
        assert_eq!(unlabelled.correct, None);

        let score = Answer::Score {
            score: 1.9,
            legend: BTreeMap::new(),
            probabilities: BTreeMap::from([
                ("0".to_owned(), Probability::new(0.1).unwrap()),
                ("1".to_owned(), Probability::new(0.2).unwrap()),
                ("2".to_owned(), Probability::new(0.7).unwrap()),
            ]),
            confidence: Confidence::new(0.6).unwrap(),
        };
        assert_eq!(Judgment::of_answer(&score, Some("2")).predicted, "2");
        let noul = Answer::Noul {
            noul: Probability::new(0.3).unwrap(),
        };
        let n = Judgment::of_answer(&noul, Some("no"));
        assert_eq!(n.predicted, "no");
        assert!((n.confidence - 0.7).abs() < 1e-12);

        let m = QuestionMetrics::summarise([&right, &wrong, &unlabelled], ECE_BINS);
        assert_eq!(m.labelled, 2);
        assert_eq!(m.correct, 1);
        assert!((m.accuracy.unwrap() - 0.5).abs() < 1e-12);
        assert_eq!(m.confusion["sales"]["billing"], 1);
        // The wrong case expected an option never offered: its Brier is
        // the plain score plus one.
        let plain = metrics::brier(&[(false, 0.8), (false, 0.2)]);
        let expected_brier = (metrics::brier(&[(true, 0.8), (false, 0.2)]) + plain + 1.0) / 2.0;
        assert!((m.brier.unwrap() - expected_brier).abs() < 1e-12);
        assert_eq!(m.confidence_when_right, Some(0.7));
        assert_eq!(m.confidence_when_wrong, Some(0.7));

        let l = Latency::of(&[10.0, 30.0, 20.0]);
        assert_eq!(l.p50_ms, Some(20.0));
        assert_eq!(l.mean_ms, Some(20.0));
    }

    #[test]
    fn an_unknown_answer_is_graded_as_a_miss_not_dropped() {
        let rank = Answer::Unknown(json!({ "type": "rank", "ranking": ["billing"] }));
        for label in ["", "billing"] {
            let j = Judgment::of_answer(&rank, Some(label));
            assert_eq!(j.predicted, "<rank>");
            assert_eq!(j.expected.as_deref(), Some(label));
            assert_eq!(j.correct, Some(false), "{label:?}");
            assert_eq!(j.p_expected, Some(0.0), "{label:?}");
            assert!(j.confidence.abs() < f64::EPSILON);
            assert!(j.probabilities.is_empty());
            assert!(j.asked);
        }
        let unlabelled = Judgment::of_answer(&rank, None);
        assert_eq!(unlabelled.correct, None);
        assert_eq!(unlabelled.p_expected, None);
        // A hostile kind is escaped in the prediction, as in an error.
        let hostile = Answer::Unknown(json!({ "type": format!("a\nb{}", "x".repeat(100)) }));
        let predicted = Judgment::of_answer(&hostile, None).predicted;
        assert!(!predicted.contains('\n'), "{predicted:?}");
        assert!(predicted.chars().count() <= 66, "{predicted:?}");

        let m =
            QuestionMetrics::summarise([&Judgment::of_answer(&rank, Some("billing"))], ECE_BINS);
        assert_eq!((m.labelled, m.correct), (1, 0));
        assert_eq!(m.accuracy, Some(0.0));
        // No distribution, and the label is not in it: a Brier of 1.0.
        assert_eq!(m.brier, Some(1.0));
        // A (0.0, wrong) pair is perfectly calibrated: it pulls the ECE and
        // the confidence when wrong toward zero.
        assert_eq!(m.ece, Some(0.0));
        assert_eq!(m.confidence_when_wrong, Some(0.0));
        assert_eq!(m.confidence_when_right, None);
        assert_eq!(m.confusion["billing"]["<rank>"], 1);
    }
}
