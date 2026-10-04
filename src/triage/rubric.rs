//! The triage rubric: the words of every question, as a `.jud` document.
//!
//! Which questions signalman asks, and what type each one is, is code: the
//! routing policy reads `owner`, `impact`, `actionable`, `duplicate_of` and
//! `caused_by_change` through typed handles. What each question says is
//! data, because it is what a team tunes to its own vocabulary and alert
//! sources. That data is a rubric in the judgment crate's `.jud` format
//! (`judgment::jud`), so the questions are written in the wire's own shape,
//! in the order the model sees them, reviewed as YAML, and named by a
//! content fingerprint that any tool computes the same way. (An
//! instruction's parts are a JSON object, sent with sorted keys.)
//!
//! The format holds one fixed request; signalman's request varies with the
//! alert. [`TriageRubric`] reads the rubric as the request for an alert that
//! carries everything, and [`TriageQuestions`](super::TriageQuestions)
//! derives each real request from it: the owner and duplicate options are
//! the candidates and open incidents of the moment, with only the no-match
//! option read from the rubric; the `catalog` and `context` instruction
//! parts, and the `duplicate_of` and `caused_by_change` questions, are sent
//! only when the alert carries what they are about. `docs/triage.md` (the
//! rubric section) states the contract; [`TriageRubric::from_rubric`]
//! enforces it, so a rubric that loads is one every alert can be asked.

use std::path::Path;
use std::sync::LazyLock;

use judgment::jud::Rubric;
use serde_json::Value;

use super::questions::{Impact, NO_DUPLICATE, NONE_OF_THESE};
use crate::question::Question;

/// The built-in rubric, compiled in: what signalman asks when the
/// configuration names no other.
pub const BUILTIN: &str = include_str!("triage.jud");

/// Id of the owner question.
pub const OWNER: &str = "owner";
/// Id of the impact question.
pub const IMPACT: &str = "impact";
/// Id of the actionable question.
pub const ACTIONABLE: &str = "actionable";
/// Id of the duplicate question.
pub const DUPLICATE_OF: &str = "duplicate_of";
/// Id of the caused-by-change question.
pub const CAUSED_BY_CHANGE: &str = "caused_by_change";

/// The owner instruction part sent only when the catalog resolved the
/// alert's component.
pub const CATALOG_PART: &str = "catalog";
/// The impact instruction part sent only when other alerts are firing.
pub const CONTEXT_PART: &str = "context";

/// Every question signalman asks, with the primitive the policy reads it as.
const QUESTIONS: [(&str, &str); 5] = [
    (OWNER, "choice"),
    (IMPACT, "score"),
    (ACTIONABLE, "noul"),
    (DUPLICATE_OF, "choice"),
    (CAUSED_BY_CHANGE, "noul"),
];

/// Where the built-in rubric says it came from, in errors and reports.
const BUILTIN_SOURCE: &str = "the built-in rubric";

static BUILTIN_RUBRIC: LazyLock<TriageRubric> = LazyLock::new(|| {
    // Compiled-in data, so a failure here is a defect in this file, not an
    // input to handle; `the_builtin_rubric_is_valid` proves it parses.
    #[allow(clippy::expect_used)]
    TriageRubric::parse(BUILTIN, BUILTIN_SOURCE).expect("the built-in rubric is valid")
});

/// A rubric checked against what signalman asks: exactly its five questions,
/// each of the primitive the policy reads, every instruction an object with
/// a non-empty `question`, the no-match options present, four impact levels,
/// and no thresholds.
#[derive(Debug, Clone, PartialEq)]
pub struct TriageRubric {
    rubric: Rubric,
    source: String,
}

impl TriageRubric {
    /// The compiled-in rubric ([`BUILTIN`]).
    pub fn builtin() -> &'static Self {
        &BUILTIN_RUBRIC
    }

    /// Parse a rubric document (YAML or JSON); `source` names it in errors.
    pub fn parse(text: &str, source: &str) -> Result<Self, String> {
        let rubric = Rubric::parse(text).map_err(|e| format!("{source}: {e}"))?;
        Self::from_rubric(rubric, source)
    }

    /// Read and parse the rubric at `path`.
    pub fn read(path: &Path) -> Result<Self, String> {
        let source = path.display().to_string();
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read the rubric {source}: {e}"))?;
        Self::parse(&text, &source)
    }

    /// Check a parsed rubric against signalman's contract.
    pub fn from_rubric(rubric: Rubric, source: &str) -> Result<Self, String> {
        let fail = |reason: String| Err(format!("{source}: {reason}"));
        let named = |reason: String| format!("{source}: {reason}");
        if !rubric.policy.gates.is_empty() || rubric.policy.tuning.is_some() {
            return fail(
                "a rubric `policy` or `tuning` would be a second source of thresholds; signalman reads them from `[policy]` in the configuration file"
                    .to_owned(),
            );
        }
        let asked = || {
            QUESTIONS
                .iter()
                .map(|(id, _)| format!("`{id}`"))
                .collect::<Vec<_>>()
                .join(", ")
        };
        for (id, question) in rubric.questions.iter() {
            let Some((_, kind)) = QUESTIONS.iter().find(|(known, _)| *known == id) else {
                return fail(format!(
                    "`{id}` is not a question signalman asks; it asks {}",
                    asked()
                ));
            };
            if question.kind() != *kind {
                return fail(format!(
                    "`{id}` is a {}, and signalman reads it as a {kind}",
                    question.kind()
                ));
            }
            check_instructions(id, question).map_err(named)?;
        }
        for (id, _) in QUESTIONS {
            if rubric.questions.get(id).is_none() {
                return fail(format!(
                    "the rubric has no `{id}` question; signalman asks {}",
                    asked()
                ));
            }
        }
        no_match_description(&rubric, OWNER, NONE_OF_THESE).map_err(named)?;
        no_match_description(&rubric, DUPLICATE_OF, NO_DUPLICATE).map_err(named)?;
        if let Some(Question::Score { criteria, .. }) = rubric.questions.get(IMPACT) {
            if criteria.len() != Impact::LEVELS.len() {
                return fail(format!(
                    "`impact` must have exactly {} levels, lowest first (none, minor, major, outage); it has {}",
                    Impact::LEVELS.len(),
                    criteria.len()
                ));
            }
            if let Some(index) = criteria.iter().position(|l| !non_empty_text(l)) {
                return fail(format!(
                    "`impact` level {index} must be text: it is the label a note and the outcome carry"
                ));
            }
        }
        Ok(Self {
            rubric,
            source: source.to_owned(),
        })
    }

    /// The rubric's `id`.
    pub fn id(&self) -> &str {
        &self.rubric.id
    }

    /// Where it was read from: a path, or `the built-in rubric`.
    pub fn source(&self) -> &str {
        &self.source
    }

    /// The `.jud` fingerprint of its questions (`sha256:…`): the identity of
    /// what the model is asked for an alert that carries everything.
    pub fn fingerprint(&self) -> String {
        self.rubric.fingerprint()
    }

    /// The fingerprint of what the rubric contributes to a request
    /// (`sha256:…`): its questions with the options supplied per alert left
    /// out, so `owner` and `duplicate_of` keep only their no-match option.
    /// It changes with every word that is sent and with the order of the
    /// questions, and not with an example option, which is never sent. The
    /// evaluation harness's `questions_fingerprint` is built on it.
    pub fn asked_fingerprint(&self) -> String {
        let mut questions = serde_json::to_value(&self.rubric.questions).unwrap_or(Value::Null);
        for (id, key) in [(OWNER, NONE_OF_THESE), (DUPLICATE_OF, NO_DUPLICATE)] {
            if let Some(criteria) = questions
                .get_mut(id)
                .and_then(|q| q.get_mut("criteria"))
                .and_then(Value::as_object_mut)
            {
                criteria.retain(|option, _| option == key);
            }
        }
        judgment::eval::canonical::fingerprint(&questions)
    }

    /// The underlying `.jud` rubric.
    pub fn jud(&self) -> &Rubric {
        &self.rubric
    }

    /// This rubric with `part` left out of every question's instructions:
    /// the request as it was before a part was added. The committed
    /// evaluation run recorded before the state rule existed is replayed
    /// against `without_part("rule")` of the built-in rubric.
    pub fn without_part(&self, part: &str) -> Result<Self, String> {
        let mut questions = crate::Questions::new();
        for (id, question) in self.rubric.questions.iter() {
            let mut question = question.clone();
            let (Question::Noul { instructions, .. }
            | Question::Choice { instructions, .. }
            | Question::Score { instructions, .. }) = &mut question;
            if let Some(parts) = instructions.as_object_mut() {
                parts.remove(part);
            }
            questions
                .add(id, question)
                .map_err(|e| format!("{}: {e}", self.source))?;
        }
        let rubric = Rubric {
            questions,
            ..self.rubric.clone()
        };
        Self::from_rubric(rubric, &format!("{} without `{part}`", self.source))
    }

    /// The questions, in the rubric's order: the wire order of every request.
    pub(crate) fn questions(&self) -> impl Iterator<Item = (&str, &Question)> {
        self.rubric.questions.iter()
    }

    /// The description of a dynamic Choice's no-match option, read from the
    /// rubric. Checked present by [`Self::from_rubric`].
    pub(crate) fn no_match(&self, id: &str, key: &str) -> Value {
        match self.rubric.questions.get(id) {
            Some(Question::Choice { criteria, .. }) => {
                criteria.get(key).cloned().unwrap_or(Value::Null)
            }
            _ => Value::Null,
        }
    }
}

fn non_empty_text(value: &Value) -> bool {
    value.as_str().is_some_and(|s| !s.trim().is_empty())
}

/// Every instruction is an object of named parts: a non-empty `question`,
/// and any other parts as non-empty text. One shape for every question, so
/// a reader of a request finds the same keys in each and the parts that
/// depend on the alert can be left out by name.
fn check_instructions(id: &str, question: &Question) -> Result<(), String> {
    let instructions = match question {
        Question::Noul { instructions, .. }
        | Question::Choice { instructions, .. }
        | Question::Score { instructions, .. } => instructions,
    };
    let Some(parts) = instructions.as_object() else {
        return Err(format!(
            "`{id}.instructions` must be an object of named parts, with at least `question`"
        ));
    };
    if !parts.get("question").is_some_and(non_empty_text) {
        return Err(format!(
            "`{id}.instructions.question` must be non-empty text"
        ));
    }
    if let Some((part, _)) = parts.iter().find(|(_, text)| !non_empty_text(text)) {
        return Err(format!(
            "`{id}.instructions.{part}` must be non-empty text; leave a part out to stop sending it"
        ));
    }
    Ok(())
}

/// A dynamic Choice's no-match option is the one option the rubric gives
/// it: it must be there, described.
fn no_match_description(rubric: &Rubric, id: &str, key: &str) -> Result<(), String> {
    match rubric.questions.get(id) {
        Some(Question::Choice { criteria, .. })
            if criteria.get(key).is_some_and(non_empty_text) =>
        {
            Ok(())
        }
        _ => Err(format!(
            "`{id}` must offer `{key}` with a description: it is the no-match option, the one option of `{id}` read from the rubric; the others are supplied per alert"
        )),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn the_builtin_rubric_is_valid() {
        let rubric = TriageRubric::parse(BUILTIN, "built-in").unwrap();
        assert_eq!(rubric.id(), "signalman-triage");
        let ids: Vec<&str> = rubric.questions().map(|(id, _)| id).collect();
        assert_eq!(
            ids,
            [OWNER, IMPACT, ACTIONABLE, DUPLICATE_OF, CAUSED_BY_CHANGE]
        );
        assert_eq!(TriageRubric::builtin().jud(), rubric.jud());
        assert_eq!(TriageRubric::builtin().source(), "the built-in rubric");
        assert!(rubric.fingerprint().starts_with("sha256:"));
    }

    #[test]
    fn the_builtin_example_owners_are_the_builtin_fallback_teams() {
        // The rubric shows the request with the built-in fallback list; this
        // keeps the two from drifting apart.
        let Some(Question::Choice { criteria, .. }) =
            TriageRubric::builtin().jud().questions.get(OWNER)
        else {
            panic!("owner is a Choice");
        };
        let rubric: Vec<(&str, &str)> = criteria
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str().unwrap()))
            .collect();
        let teams = crate::triage::OwnerCandidates::from_teams();
        let fallback: Vec<(&str, &str)> = teams
            .iter()
            .map(|c| (c.key.as_str(), c.description.as_str()))
            .collect();
        assert_eq!(rubric, fallback);
    }

    /// The built-in rubric with one edit applied to its text.
    fn edited(from: &str, to: &str) -> Result<TriageRubric, String> {
        assert!(BUILTIN.contains(from), "{from}");
        TriageRubric::parse(&BUILTIN.replacen(from, to, 1), "edited")
    }

    #[test]
    fn a_rubric_that_breaks_the_contract_is_refused_by_name() {
        let cases = [
            (
                "  caused_by_change:\n    type: noul",
                "  caused_by_chang:\n    type: noul",
                "`caused_by_chang` is not a question signalman asks",
            ),
            (
                "      question: Which team should own the first response to `alert`?\n",
                "",
                "`owner.instructions.question` must be non-empty text",
            ),
            (
                "      none_of_these: Not clearly attributable to any listed team from the information given\n",
                "",
                "`owner` must offer `none_of_these`",
            ),
            (
                "      none: \"`alert` is a new, separate problem not covered by any open incident\"\n",
                "      nope: x\n",
                "`duplicate_of` must offer `none`",
            ),
            (
                "      - Degraded experience for a small subset of users or one non-critical feature\n",
                "",
                "exactly 4 levels",
            ),
        ];
        for (from, to, needle) in cases {
            let err = edited(from, to).unwrap_err();
            assert!(err.starts_with("edited: "), "{err}");
            assert!(err.contains(needle), "{needle}: {err}");
        }
    }

    #[test]
    fn a_question_of_the_wrong_primitive_is_refused() {
        let text = BUILTIN.replacen(
            "  caused_by_change:\n    type: noul\n",
            "  caused_by_change:\n    type: score\n    criteria: [no, yes]\n",
            1,
        );
        let err = TriageRubric::parse(&text, "edited").unwrap_err();
        assert!(
            err.contains("`caused_by_change` is a score, and signalman reads it as a noul"),
            "{err}"
        );
    }

    #[test]
    fn a_missing_question_is_refused() {
        let start = BUILTIN.find("  caused_by_change:").unwrap();
        let err = TriageRubric::parse(&BUILTIN[..start], "edited").unwrap_err();
        assert!(
            err.contains("the rubric has no `caused_by_change` question"),
            "{err}"
        );
    }

    #[test]
    fn thresholds_in_the_rubric_are_refused() {
        let text = format!("{BUILTIN}\npolicy:\n  actionable:\n    threshold: 0.25\n");
        let err = TriageRubric::parse(&text, "edited").unwrap_err();
        assert!(err.contains("`[policy]`"), "{err}");
        let text = format!("{BUILTIN}\ntuning:\n  model: jev-1.13.0\n");
        assert!(
            TriageRubric::parse(&text, "edited")
                .unwrap_err()
                .contains("`[policy]`")
        );
    }

    #[test]
    fn a_part_can_be_left_out_and_an_empty_one_is_refused() {
        // Leaving `rule` out of a question is how it stops being sent there.
        edited(
            "      rule: *rule\n    criteria:\n      true:",
            "    criteria:\n      true:",
        )
        .unwrap();
        let err = edited(
            "      question: What is the current user-facing impact described by `alert`?\n",
            "      question: What is the current user-facing impact described by `alert`?\n      focus: \"\"\n",
        )
        .unwrap_err();
        assert!(
            err.contains("`impact.instructions.focus` must be non-empty text"),
            "{err}"
        );
    }

    #[test]
    fn a_part_is_left_out_of_every_question() {
        let off = TriageRubric::builtin().without_part("rule").unwrap();
        for (id, question) in off.questions() {
            let json = serde_json::to_value(question).unwrap();
            assert!(json["instructions"].get("rule").is_none(), "{id}");
            assert!(json["instructions"]["question"].is_string(), "{id}");
        }
        assert_ne!(off.fingerprint(), TriageRubric::builtin().fingerprint());
        assert!(off.source().ends_with("without `rule`"), "{}", off.source());
        // `question` is required: leaving it out is refused.
        assert!(TriageRubric::builtin().without_part("question").is_err());
    }

    #[test]
    fn an_example_option_is_not_part_of_what_is_asked() {
        let renamed = edited(
            "      INC-4821: \"Checkout 5xx spike [Major]: payments-gateway returning errors\"",
            "      INC-1: an example",
        )
        .unwrap();
        assert_ne!(renamed.fingerprint(), TriageRubric::builtin().fingerprint());
        assert_eq!(
            renamed.asked_fingerprint(),
            TriageRubric::builtin().asked_fingerprint()
        );
        let reworded = edited(
            "      none: \"`alert` is a new, separate problem not covered by any open incident\"",
            "      none: a new problem",
        )
        .unwrap();
        assert_ne!(
            reworded.asked_fingerprint(),
            TriageRubric::builtin().asked_fingerprint()
        );
    }

    #[test]
    fn a_format_error_names_the_source_and_the_line() {
        let err = TriageRubric::parse("jud: 1\nkind: rubric\nid: x\nquestions: [\n", "my.jud")
            .unwrap_err();
        assert!(err.starts_with("my.jud: "), "{err}");
        assert!(err.contains("line"), "{err}");
    }
}
