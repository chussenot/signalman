//! The evaluation harness: replay labelled alerts through the triage
//! questions, grade every judgment and the resulting decision, and report
//! accuracy, calibration and latency.
//!
//! Two modes share one grader. `run` calls the model and can record the raw
//! response of every graded case; `replay` re-grades recorded responses
//! under the current configuration without calling the model, which is how
//! thresholds and wording are tuned: the judgments do not change when the
//! policy does (decision 0002), so inference need not be repeated to see a
//! different decision.
//!
//! A live run keeps going past a case whose answer does not fit its
//! questions (an option the question never offered, a Score off its scale):
//! the client refuses such a response, and one bad answer should not throw
//! away the rest of a paid run. The case is listed in [`Report::failed`]
//! with the error and its request id and is not graded, so the accuracy is
//! over the graded cases. Under `--record` it has no `<id>.json` (one left
//! by an earlier run is removed, so a replay cannot grade it as this run's
//! answer) and is listed in [`FAILED_FILE`] instead, which a replay reads
//! back into [`Report::failed`]. Any other error (the network, a key, a case
//! that does not build) stops the run. A replay stays strict otherwise: a
//! case with neither a recording nor a failed entry is an error, and so is
//! a recording that no longer fits, which means the setup changed under it;
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
use serde_json::json;

use crate::answer::Response;
use crate::triage::{
    Alert, Decision, Impact, NO_DUPLICATE, OwnerCandidates, Policy, TriageAnswers, TriageQuestions,
    TriageRubric, decide,
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
    /// Which set the case belongs to; `development` unless the file says
    /// otherwise. Thresholds are tuned on the development set and the
    /// held-out set is graded once with the chosen policy, so a number
    /// reported on it was not fitted to it.
    #[serde(default)]
    pub split: Split,
    /// Where the labels came from, so a report can say what its accuracy is
    /// an accuracy against. Absent is reported as `unspecified`, not hidden.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<Provenance>,
    /// Why the labels are what they are, for the next person to read the
    /// case. Free text, never sent to the model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rationale: Option<String>,
}

/// The set a case belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Split {
    /// Used to choose thresholds and wording.
    #[default]
    Development,
    /// Graded once with a policy chosen elsewhere.
    HeldOut,
}

impl Split {
    /// The name used in files and on the command line.
    pub const fn key(self) -> &'static str {
        match self {
            Self::Development => "development",
            Self::HeldOut => "held-out",
        }
    }
}

impl std::str::FromStr for Split {
    type Err = String;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "development" => Ok(Self::Development),
            "held-out" => Ok(Self::HeldOut),
            other => Err(format!(
                "unknown split {other:?}: use development or held-out"
            )),
        }
    }
}

/// How a case's labels were produced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Provenance {
    /// The method.
    pub method: Method,
    /// Who or what, in a sentence: the on-call rotation that reviewed the
    /// week, the script that generated the variants, the incident the
    /// outcome was read from.
    pub source: String,
}

/// The ways a label comes to exist, from strongest to weakest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Method {
    /// What happened: the incident the alert was attached to, the team
    /// that resolved it, the page that was or was not needed.
    ObservedOutcome,
    /// A person who would have handled the alert said what was right.
    HumanLabelled,
    /// Written by the harness's authors or generated, without independent
    /// review; the cases shipped in `examples/eval` are this.
    AuthorSynthetic,
    /// Not recorded.
    Unspecified,
}

impl Method {
    /// The name used in files and reports.
    pub const fn key(self) -> &'static str {
        match self {
            Self::ObservedOutcome => "observed-outcome",
            Self::HumanLabelled => "human-labelled",
            Self::AuthorSynthetic => "author-synthetic",
            Self::Unspecified => "unspecified",
        }
    }
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
    /// 95% Wilson interval around `accuracy`, `(low, high)`.
    pub accuracy_interval95: Option<(f64, f64)>,
    /// `expected -> reached -> count`.
    pub confusion: BTreeMap<String, BTreeMap<String, usize>>,
}

/// The file, beside the recordings, that lists the cases a recorded live
/// run could not grade: one [`FailedCase`] as JSON per line. Its extension
/// is not `.json`, so nothing that reads a directory of recordings (a
/// [`judgment::backend::Replay`], say) takes it for one.
pub const FAILED_FILE: &str = "failed.jsonl";

/// The file, beside the recordings, that says what a recorded run was: when,
/// which model was asked, and the fingerprint of the question texts and
/// owner candidates the answers were given to. A replay compares that
/// fingerprint with the current one and refuses to grade old answers under
/// new questions unless told the staleness is understood.
pub const MANIFEST_FILE: &str = "run.json";

/// What a recorded run was made with. Written by [`run`] under `--record`,
/// read by [`replay`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    /// When the run finished, RFC 3339.
    pub recorded_at: String,
    /// The model asked for; the answering model is on each recording.
    pub model: String,
    /// `signalman` version that recorded.
    pub signalman_version: String,
    /// [`questions_fingerprint`] of the texts and candidates in force.
    pub questions_fingerprint: String,
    /// [`cases_fingerprint`] of the case file as read.
    pub cases_fingerprint: String,
    /// Cases attempted.
    pub cases: usize,
    /// Cases per split.
    pub splits: BTreeMap<String, usize>,
    /// Cases per provenance method.
    pub provenance: BTreeMap<String, usize>,
}

/// What a report's numbers are evidence of: live answers or a replay, under
/// which questions, from cases of which origin.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Evidence {
    /// `live` for a model call, `replay` for recorded answers.
    pub mode: &'static str,
    /// When the answers were recorded; the run itself for `live`.
    pub recorded_at: Option<String>,
    /// Fingerprint of the question texts and owner candidates the answers
    /// were graded under.
    pub questions_fingerprint: String,
    /// Fingerprint the answers were recorded under, when the recording has
    /// a manifest; `None` for a live run or an older recording.
    pub recorded_fingerprint: Option<String>,
    /// True when the answers were given to different questions than they
    /// are graded under: the numbers say how the policy reads old answers,
    /// not how the model answers the current questions.
    pub stale: bool,
    /// [`policy_fingerprint`] of the policy the decisions were made under.
    pub policy_fingerprint: String,
    /// Fingerprint of the policy frozen beside the recordings
    /// ([`POLICY_FILE`]), when there is one; `None` for a live run or a
    /// directory where nothing was frozen. Equal to `policy_fingerprint`
    /// when the report was graded under the frozen policy.
    pub frozen_policy: Option<String>,
    /// Cases per split.
    pub splits: BTreeMap<String, usize>,
    /// Cases per provenance method.
    pub provenance: BTreeMap<String, usize>,
}

/// The file, beside the recordings, that holds the policy chosen on the
/// development cases. Written by [`freeze_policy`] from a development
/// replay; a held-out replay refuses to grade without it or under any
/// other policy ([`held_out_gate`]).
pub const POLICY_FILE: &str = "policy.json";

/// The policy a development replay settled on, with what it was chosen on.
/// Freezing it is what makes a later held-out number a number about the
/// next alert rather than about the cases it was tuned on: the held-out
/// replay can only ever be graded under this policy, so the choice cannot
/// be revised after seeing the held-out result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FrozenPolicy {
    /// When it was frozen, RFC 3339.
    pub frozen_at: String,
    /// The policy itself, so the file is readable and usable on its own.
    pub policy: Policy,
    /// [`policy_fingerprint`] of it; what a held-out replay compares.
    pub policy_fingerprint: String,
    /// [`questions_fingerprint`] in force when it was chosen.
    pub questions_fingerprint: String,
    /// Whether the development replay it was chosen on was stale.
    pub stale: bool,
    /// The development result it was chosen on.
    pub chosen_on: ChosenOn,
}

/// The development figures a policy was frozen on.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChosenOn {
    /// Development cases graded.
    pub cases: usize,
    /// Of which labelled with an expected action.
    pub labelled: usize,
    /// Decision agreement on them.
    pub agreement: Option<f64>,
    /// Its 95% interval.
    pub agreement_interval95: Option<(f64, f64)>,
}

/// Fingerprint of a policy: every threshold, under sorted keys.
pub fn policy_fingerprint(policy: &Policy) -> String {
    judgment::eval::fingerprint(&serde_json::to_value(policy).unwrap_or(serde_json::Value::Null))
}

/// Fingerprint of everything that shapes a request apart from the alert:
/// the rubric (by [`TriageRubric::asked_fingerprint`], which covers every
/// word sent and the order of the questions, but not an example option that
/// is never sent) and the owner candidates. Two runs with the same
/// fingerprint asked the same questions; a replay under a different one
/// reads answers to questions that were never asked.
pub fn questions_fingerprint(rubric: &TriageRubric, candidates: &OwnerCandidates) -> String {
    judgment::eval::fingerprint(&json!({
        "rubric": rubric.asked_fingerprint(),
        "candidates": candidates.iter().collect::<Vec<_>>(),
    }))
}

/// Fingerprint of a case file's content: ids, alerts and labels.
pub fn cases_fingerprint(cases: &[Case]) -> String {
    judgment::eval::fingerprint(&serde_json::to_value(cases).unwrap_or(serde_json::Value::Null))
}

fn count_cases(cases: &[Case]) -> (BTreeMap<String, usize>, BTreeMap<String, usize>) {
    let mut splits = BTreeMap::new();
    let mut provenance = BTreeMap::new();
    for case in cases {
        *splits.entry(case.split.key().to_owned()).or_default() += 1;
        let method = case
            .provenance
            .as_ref()
            .map_or(Method::Unspecified, |p| p.method);
        *provenance.entry(method.key().to_owned()).or_default() += 1;
    }
    (splits, provenance)
}

/// A case a live run could not grade: the model's answer did not fit the
/// questions it was sent ([`judgment::Error::is_unfit`]), so the client
/// refused it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    /// What the numbers are evidence of.
    pub evidence: Evidence,
    /// Every graded case, for drill-down.
    pub graded: Vec<Graded>,
    /// Cases a live run could not grade because the answer did not fit the
    /// questions; left out of every metric above. A replay lists the ones
    /// its recorded run wrote to [`FAILED_FILE`]. Omitted from the JSON when
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
    /// The recording was made under other question texts or owner
    /// candidates than the current configuration. Grading it would report
    /// how the policy reads answers to questions that are no longer asked;
    /// record again, or pass `--stale-ok` to see that number anyway.
    #[error(
        "recording under {dir} was made {recorded_at} with question fingerprint {recorded}, the current configuration has {current}: the answers were given to other questions; record again, or replay with --stale-ok to grade them anyway"
    )]
    StaleRecording {
        /// The directory.
        dir: String,
        /// When it was recorded.
        recorded_at: String,
        /// Its fingerprint.
        recorded: String,
        /// The current fingerprint.
        current: String,
    },
    /// A held-out replay with no frozen policy beside the recordings.
    #[error(
        "no frozen policy under {dir}: the held-out cases are graded only under a policy chosen on the development cases; replay those with --split development --freeze-policy first"
    )]
    NoFrozenPolicy {
        /// The directory.
        dir: String,
    },
    /// A held-out replay under a policy other than the frozen one.
    #[error(
        "the policy under {dir} was frozen {frozen_at} with fingerprint {frozen}, the current configuration has {current}: a held-out replay is graded only under the frozen policy; replay the development cases with --freeze-policy to choose again, and know that the held-out cases were seen"
    )]
    PolicyNotFrozen {
        /// The directory.
        dir: String,
        /// When the policy was frozen.
        frozen_at: String,
        /// The frozen fingerprint.
        frozen: String,
        /// The current fingerprint.
        current: String,
    },
    /// Freezing from a replay that graded cases outside the development
    /// split, which would freeze a policy chosen on the held-out ones.
    #[error(
        "a policy is frozen from a replay of the development cases only (--split development), this one graded: {splits}"
    )]
    FreezeNeedsDevelopment {
        /// The splits graded, comma-separated.
        splits: String,
    },
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
    /// The words of every question.
    pub rubric: &'a TriageRubric,
    /// Owner candidates for every case (no catalog in the harness).
    pub candidates: &'a OwnerCandidates,
    /// Routing thresholds.
    pub policy: &'a Policy,
}

/// Call the model for every case, grade, and optionally record each graded
/// response as `<record>/<id>.json`.
///
/// A case whose answer does not fit its questions is listed in
/// [`Report::failed`] and the run continues; it is not graded. Under
/// `record` its `<id>.json`, if an earlier run left one, is removed, and
/// every failed case is written to `<record>/`[`FAILED_FILE`] (removed when
/// none failed), so [`replay`] reports the same cases as failed. Any other
/// error stops the run.
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
        let questions = TriageQuestions::for_alert_with_rubric(
            &case.alert,
            setup.candidates.clone(),
            setup.rubric,
        )?;
        let state = TriageQuestions::state(&case.alert);
        let started = Instant::now();
        let response = match backend.answer(&state, model, &questions.questions).await {
            Ok(r) => r,
            Err(e) if e.is_unfit() => {
                if let Some(dir) = record {
                    // An answer from an earlier run would otherwise be
                    // graded on replay as if this run had given it.
                    remove_if_present(&judgment::eval::recording_path(dir, &case.id))?;
                }
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
            // Keyed by case id, with the request's canonical fingerprint
            // and the time, so a recording says which request it answers
            // and when, as any `.jud` reader names it.
            judgment::eval::write_recording(
                dir,
                &Recording {
                    fingerprint: Some(judgment::eval::canonical::request_fingerprint(
                        &state,
                        &questions.questions,
                    )),
                    recorded_at: Some(judgment::eval::now_rfc3339()),
                    ..Recording::new(case.id.clone(), response.clone(), elapsed_ms)
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
    let fingerprint = questions_fingerprint(setup.rubric, setup.candidates);
    let recorded_at = jiff::Timestamp::now().to_string();
    let (splits, provenance) = count_cases(cases);
    if let Some(dir) = record {
        write_failed(dir, &failed)?;
        write_manifest(
            dir,
            &Manifest {
                recorded_at: recorded_at.clone(),
                model: model.to_owned(),
                signalman_version: env!("CARGO_PKG_VERSION").to_owned(),
                questions_fingerprint: fingerprint.clone(),
                cases_fingerprint: cases_fingerprint(cases),
                cases: cases.len(),
                splits: splits.clone(),
                provenance: provenance.clone(),
            },
        )?;
    }
    Ok(report(
        graded,
        failed,
        Evidence {
            mode: "live",
            recorded_at: Some(recorded_at),
            questions_fingerprint: fingerprint,
            recorded_fingerprint: None,
            stale: false,
            policy_fingerprint: policy_fingerprint(setup.policy),
            frozen_policy: None,
            splits,
            provenance,
        },
    ))
}

/// Grade recorded responses under the current setup without calling the
/// model. Every case needs `<dir>/<id>.json`, or an entry in
/// `<dir>/`[`FAILED_FILE`], which puts it in [`Report::failed`] as the
/// recorded run did.
///
/// When `<dir>/`[`MANIFEST_FILE`] records a different question fingerprint
/// than the current texts and candidates give, the answers were given to
/// other questions and the replay fails with [`Error::StaleRecording`],
/// unless `stale_ok`, in which case it grades them and the report's
/// [`Evidence::stale`] says so. A directory without a manifest, recorded
/// before manifests existed, is graded with `recorded_fingerprint` unknown.
///
/// The replay grades under `setup.policy` whatever is frozen beside the
/// recordings; [`Evidence::frozen_policy`] says whether that was the frozen
/// one. Refusing a held-out replay under another policy is
/// [`held_out_gate`], which the caller runs first when the cases are the
/// held-out split.
pub fn replay(dir: &Path, cases: &[Case], setup: &Setup<'_>, stale_ok: bool) -> Result<Report> {
    let fingerprint = questions_fingerprint(setup.rubric, setup.candidates);
    let manifest = read_manifest(dir)?;
    let stale = manifest
        .as_ref()
        .is_some_and(|m| m.questions_fingerprint != fingerprint);
    if let (true, false, Some(m)) = (stale, stale_ok, manifest.as_ref()) {
        return Err(Error::StaleRecording {
            dir: dir.display().to_string(),
            recorded_at: m.recorded_at.clone(),
            recorded: m.questions_fingerprint.clone(),
            current: fingerprint,
        });
    }
    let recorded_failures = read_failed(dir)?;
    let mut graded = Vec::with_capacity(cases.len());
    let mut failed = Vec::new();
    for case in cases {
        if let Some(f) = recorded_failures.get(&case.id) {
            failed.push(f.clone());
            continue;
        }
        let rec = judgment::eval::read_recording(dir, &case.id)?;
        let questions = TriageQuestions::for_alert_with_rubric(
            &case.alert,
            setup.candidates.clone(),
            setup.rubric,
        )?;
        graded.push(grade(
            case,
            &questions,
            &rec.response,
            setup.policy,
            rec.elapsed_ms,
        )?);
    }
    let (splits, provenance) = count_cases(cases);
    let frozen = read_frozen_policy(dir)?;
    Ok(report(
        graded,
        failed,
        Evidence {
            mode: "replay",
            recorded_at: manifest.as_ref().map(|m| m.recorded_at.clone()),
            questions_fingerprint: fingerprint,
            recorded_fingerprint: manifest.map(|m| m.questions_fingerprint),
            stale,
            policy_fingerprint: policy_fingerprint(setup.policy),
            frozen_policy: frozen.map(|f| f.policy_fingerprint),
            splits,
            provenance,
        },
    ))
}

/// Freeze the policy a development replay was graded under, as
/// `<dir>/`[`POLICY_FILE`], recording the development figures it was
/// chosen on. `report` must come from a replay of the development cases
/// only ([`Error::FreezeNeedsDevelopment`] otherwise): a policy chosen with
/// the held-out cases in view is not one the held-out replay can vouch
/// for. Freezing again replaces the file; the held-out cases have then
/// been seen once per freeze, which the report's `frozen_at` dates.
pub fn freeze_policy(dir: &Path, policy: &Policy, report: &Report) -> Result<FrozenPolicy> {
    let e = &report.evidence;
    if e.splits.keys().any(|k| k != Split::Development.key()) {
        return Err(Error::FreezeNeedsDevelopment {
            splits: e.splits.keys().cloned().collect::<Vec<_>>().join(", "),
        });
    }
    let frozen = FrozenPolicy {
        frozen_at: jiff::Timestamp::now().to_string(),
        policy: policy.clone(),
        policy_fingerprint: policy_fingerprint(policy),
        questions_fingerprint: e.questions_fingerprint.clone(),
        stale: e.stale,
        chosen_on: ChosenOn {
            cases: report.cases,
            labelled: report.decision.labelled,
            agreement: report.decision.accuracy,
            agreement_interval95: report.decision.accuracy_interval95,
        },
    };
    write_json(&dir.join(POLICY_FILE), &frozen)?;
    Ok(frozen)
}

/// The policy frozen under `dir`, if any.
pub fn read_frozen_policy(dir: &Path) -> Result<Option<FrozenPolicy>> {
    read_json(&dir.join(POLICY_FILE))
}

/// What a held-out replay of `dir` must pass before grading: a policy is
/// frozen there and `policy` is that policy. Returns the frozen record so
/// the caller can say when and on what it was chosen.
pub fn held_out_gate(dir: &Path, policy: &Policy) -> Result<FrozenPolicy> {
    let Some(frozen) = read_frozen_policy(dir)? else {
        return Err(Error::NoFrozenPolicy {
            dir: dir.display().to_string(),
        });
    };
    let current = policy_fingerprint(policy);
    if frozen.policy_fingerprint != current {
        return Err(Error::PolicyNotFrozen {
            dir: dir.display().to_string(),
            frozen_at: frozen.frozen_at,
            frozen: frozen.policy_fingerprint,
            current,
        });
    }
    Ok(frozen)
}

fn write_manifest(dir: &Path, manifest: &Manifest) -> Result<()> {
    write_json(&dir.join(MANIFEST_FILE), manifest)
}

fn read_manifest(dir: &Path) -> Result<Option<Manifest>> {
    read_json(&dir.join(MANIFEST_FILE))
}

/// Write `value` as pretty JSON with a final newline, so the file diffs
/// line by line when committed.
fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let mut text = serde_json::to_string_pretty(value).map_err(|source| Error::Json {
        context: path.display().to_string(),
        source,
    })?;
    text.push('\n');
    std::fs::write(path, text).map_err(|source| Error::Io {
        path: path.display().to_string(),
        source,
    })
}

/// Read `path` as JSON; a file that is not there is `None`.
fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(Error::Io {
                path: path.display().to_string(),
                source,
            });
        }
    };
    serde_json::from_str(&text)
        .map(Some)
        .map_err(|source| Error::Json {
            context: path.display().to_string(),
            source,
        })
}

/// Remove `path`; a file that is not there is already removed.
fn remove_if_present(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(Error::Io {
            path: path.display().to_string(),
            source: e,
        }),
        _ => Ok(()),
    }
}

/// Write `failed` to `<dir>/`[`FAILED_FILE`], one case per line, or remove
/// the file when nothing failed, so it always describes the latest run.
fn write_failed(dir: &Path, failed: &[FailedCase]) -> Result<()> {
    let path = dir.join(FAILED_FILE);
    if failed.is_empty() {
        return remove_if_present(&path);
    }
    std::fs::create_dir_all(dir).map_err(|source| Error::Io {
        path: dir.display().to_string(),
        source,
    })?;
    let mut text = String::new();
    for f in failed {
        let line = serde_json::to_string(f).map_err(|source| Error::Json {
            context: path.display().to_string(),
            source,
        })?;
        text.push_str(&line);
        text.push('\n');
    }
    std::fs::write(&path, text).map_err(|source| Error::Io {
        path: path.display().to_string(),
        source,
    })
}

/// The cases `<dir>/`[`FAILED_FILE`] lists, by id; none when there is no
/// such file.
fn read_failed(dir: &Path) -> Result<BTreeMap<String, FailedCase>> {
    let path = dir.join(FAILED_FILE);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(source) => {
            return Err(Error::Io {
                path: path.display().to_string(),
                source,
            });
        }
    };
    let mut out = BTreeMap::new();
    for (i, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let f: FailedCase = serde_json::from_str(line).map_err(|source| Error::Json {
            context: format!("{}:{}", path.display(), i + 1),
            source,
        })?;
        out.insert(f.id.clone(), f);
    }
    Ok(out)
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
pub fn report(graded: Vec<Graded>, failed: Vec<FailedCase>, evidence: Evidence) -> Report {
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
        decision.accuracy_interval95 =
            metrics::wilson_interval(decision.correct, decision.labelled, judgment::eval::Z_95);
    }
    Report {
        cases: graded.len(),
        models,
        questions,
        decision,
        latency: Latency::of(&latencies),
        input_tokens,
        output_tokens,
        evidence,
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
        let interval = |v: Option<(f64, f64)>| {
            v.map_or("         -".to_owned(), |(l, h)| format!("{l:.2}..{h:.2}"))
        };
        let _ = writeln!(
            s,
            "decision  labelled {}  agreement {}  95% {}",
            self.decision.labelled,
            fmt(self.decision.accuracy),
            interval(self.decision.accuracy_interval95)
        );
        let e = &self.evidence;
        let counts = |m: &BTreeMap<String, usize>| {
            m.iter()
                .map(|(k, n)| format!("{k} {n}"))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let _ = writeln!(
            s,
            "evidence  {}{}  questions {}{}",
            e.mode,
            e.recorded_at
                .as_deref()
                .map_or(String::new(), |at| format!(" recorded {at}")),
            e.questions_fingerprint,
            match (&e.recorded_fingerprint, e.stale) {
                (None, _) if e.mode == "replay" => "  (recording has no manifest)".to_owned(),
                (Some(r), true) => format!(
                    "  STALE: recorded under {r}; these answers were given to other questions"
                ),
                _ => String::new(),
            }
        );
        let _ = writeln!(
            s,
            "policy    {}{}",
            e.policy_fingerprint,
            match &e.frozen_policy {
                Some(f) if *f == e.policy_fingerprint => "  (frozen)".to_owned(),
                Some(f) => format!("  not the frozen one ({f}); a held-out replay would refuse it"),
                None => String::new(),
            }
        );
        let _ = writeln!(
            s,
            "origin    split {}   labels {}",
            counts(&e.splits),
            counts(&e.provenance)
        );
        let _ = writeln!(s);
        let _ = writeln!(
            s,
            "{:<18} {:>3} {:>5} {:>10} {:>6} {:>5} {:>10} {:>10}",
            "question", "n", "acc", "acc 95%", "brier", "ece", "conf|right", "conf|wrong"
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
                "{:<18} {:>3} {:>5} {:>10} {:>6} {:>5} {:>10} {:>10}",
                qid,
                m.labelled,
                fmt(m.accuracy),
                interval(m.accuracy_interval95),
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
