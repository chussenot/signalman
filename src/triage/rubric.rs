//! The triage rubric: every question, when it is asked, and the routing
//! thresholds, as a `.jud` document (version 1.1).
//!
//! Which questions signalman asks, and what type each one is, is code: the
//! routing policy reads `owner`, `impact`, `actionable`, `duplicate_of` and
//! `caused_by_change` through typed handles. Everything else is data, in a
//! rubric in the judgment crate's `.jud` format (`judgment::jud`): the
//! words, written in the wire's own shape and in the order the model sees
//! them; when a question or an instruction part is sent (`when`,
//! `part_when`, state paths into `{alert: …}`); that `owner` and
//! `duplicate_of` are asked over options supplied per alert
//! (`options_from: request`); and the thresholds `decide` reads, as the
//! rubric's `policy` gates. The request for an alert is
//! `Rubric::lower(state, supplied)`, with the owner candidates and the open
//! incidents supplied here.
//!
//! [`TriageRubric::from_rubric`] holds a rubric to what the code relies on,
//! so a rubric that loads is one every alert can be asked and every answer
//! routed: `docs/triage.md` (the rubric section) states the contract.

use std::path::Path;
use std::sync::LazyLock;

use judgment::jud::{Gate, OptionsFrom, Rubric, RubricQuestion, Supplied};
use serde_json::Value;

use super::policy::Policy;
use super::questions::{Impact, NO_DUPLICATE, NONE_OF_THESE};
use super::{Alert, OwnerCandidates};
use crate::question::{Question, Questions};

/// The built-in rubric, compiled in: what signalman asks, and the
/// thresholds it routes by, when the configuration names no other.
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

/// When each question is asked, which is code like the set of questions:
/// `owner`, `impact` and `actionable` always, since `decide` reads all
/// three; `duplicate_of` only with open incidents, or it would be asked
/// over `none` alone, which the request builder refuses; and
/// `caused_by_change` only with recent changes, or a change could be
/// flagged as the cause when none is listed.
const ASKED_WHEN: [(&str, Option<&str>); 5] = [
    (OWNER, None),
    (IMPACT, None),
    (ACTIONABLE, None),
    (DUPLICATE_OF, Some("alert.open_incidents")),
    (CAUSED_BY_CHANGE, Some("alert.recent_changes")),
];

/// The owner band names `decide` reads, highest first.
const ROUTE: &str = "route";
const CONFIRM: &str = "confirm";

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

/// A rubric checked against what signalman needs, with the routing policy
/// read from its gates.
#[derive(Debug, Clone, PartialEq)]
pub struct TriageRubric {
    rubric: Rubric,
    source: String,
    policy: Policy,
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

    /// Check a parsed rubric against signalman's contract and read its
    /// policy.
    pub fn from_rubric(rubric: Rubric, source: &str) -> Result<Self, String> {
        let named = |reason: String| format!("{source}: {reason}");
        let asked = || {
            QUESTIONS
                .iter()
                .map(|(id, _)| format!("`{id}`"))
                .collect::<Vec<_>>()
                .join(", ")
        };
        for (id, rq) in &rubric.questions {
            let Some((_, kind)) = QUESTIONS.iter().find(|(known, _)| *known == id) else {
                return Err(named(format!(
                    "`{id}` is not a question signalman asks; it asks {}",
                    asked()
                )));
            };
            if rq.question.kind() != *kind {
                return Err(named(format!(
                    "`{id}` is a {}, and signalman reads it as a {kind}",
                    rq.question.kind()
                )));
            }
            check_instructions(id, &rq.question).map_err(named)?;
        }
        for (id, _) in QUESTIONS {
            if !rubric.questions.contains_key(id) {
                return Err(named(format!(
                    "the rubric has no `{id}` question; signalman asks {}",
                    asked()
                )));
            }
        }
        supplied_choice(&rubric, OWNER, NONE_OF_THESE).map_err(named)?;
        supplied_choice(&rubric, DUPLICATE_OF, NO_DUPLICATE).map_err(named)?;
        for (id, when) in ASKED_WHEN {
            if rubric.questions[id].when.as_deref() != when {
                return Err(named(match when {
                    None => format!(
                        "`{id}` is asked for every alert, so it takes no `when`: the decision reads it"
                    ),
                    Some(path) => format!(
                        "`{id}` must keep `when: {path}`: it is asked only when there is something to choose from"
                    ),
                }));
            }
        }
        let levels = impact_levels(&rubric);
        if levels.len() != Impact::LEVELS.len() {
            return Err(named(format!(
                "`impact` must have exactly {} levels, lowest first (none, minor, major, outage); it has {}",
                Impact::LEVELS.len(),
                levels.len()
            )));
        }
        if let Some(index) = levels.iter().position(|l| !non_empty_text(l)) {
            return Err(named(format!(
                "`impact` level {index} must be text: it is the label a note and the outcome carry"
            )));
        }
        let policy = policy_from(&rubric, &levels).map_err(named)?;
        Ok(Self {
            rubric,
            source: source.to_owned(),
            policy,
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

    /// The `.jud` fingerprint of its questions (`sha256:…`), declarations
    /// included: the identity of what can be asked, the same from any tool.
    /// The policy is not part of it.
    pub fn fingerprint(&self) -> String {
        self.rubric.fingerprint()
    }

    /// The routing thresholds, read from the rubric's gates.
    pub fn policy(&self) -> &Policy {
        &self.policy
    }

    /// The underlying `.jud` rubric.
    pub fn jud(&self) -> &Rubric {
        &self.rubric
    }

    /// The request for `alert`, its state being `{alert: …}`: the rubric
    /// lowered with the owner candidates and the open incidents (by id)
    /// supplied.
    pub fn lower(
        &self,
        alert: &Alert,
        state: &Value,
        candidates: &OwnerCandidates,
    ) -> Result<Questions, judgment::jud::Error> {
        let mut supplied = Supplied::new();
        supplied.insert(
            OWNER.to_owned(),
            candidates
                .iter()
                .filter(|c| c.key != NONE_OF_THESE)
                .map(|c| (c.key.clone(), Value::String(c.description.clone())))
                .collect(),
        );
        if !alert.open_incidents.is_empty() {
            // By id: a stable order whatever order incident.io listed them
            // in, and the order judgment 0.3 sent them in, so recordings
            // stay comparable.
            let mut incidents: Vec<_> = alert.open_incidents.iter().collect();
            incidents.sort_by(|a, b| a.id.cmp(&b.id));
            supplied.insert(
                DUPLICATE_OF.to_owned(),
                incidents
                    .into_iter()
                    .map(|i| (i.id.clone(), Value::String(i.summary.clone())))
                    .collect(),
            );
        }
        self.rubric.lower(state, &supplied)
    }

    /// This rubric with `part` left out of every question's instructions
    /// (and of its `part_when`): the request as it was before a part was
    /// added. The committed evaluation run recorded before the state rule
    /// existed is replayed against `without_part("rule")` of the built-in
    /// rubric.
    pub fn without_part(&self, part: &str) -> Result<Self, String> {
        let mut rubric = self.rubric.clone();
        for rq in rubric.questions.values_mut() {
            let (Question::Noul { instructions, .. }
            | Question::Choice { instructions, .. }
            | Question::Score { instructions, .. }) = &mut rq.question;
            if let Some(parts) = instructions.as_object_mut() {
                parts.remove(part);
            }
            rq.part_when.shift_remove(part);
        }
        Self::from_rubric(rubric, &format!("{} without `{part}`", self.source))
    }

    /// The description of a dynamic Choice's no-match option, read from the
    /// rubric. Checked present by [`Self::from_rubric`].
    pub(crate) fn no_match(&self, id: &str, key: &str) -> Value {
        match self.rubric.questions.get(id).map(|rq| &rq.question) {
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

fn impact_levels(rubric: &Rubric) -> Vec<Value> {
    match rubric.questions.get(IMPACT).map(|rq| &rq.question) {
        Some(Question::Score { criteria, .. }) => criteria.clone(),
        _ => Vec::new(),
    }
}

/// Every instruction is an object of named parts: a non-empty `question`,
/// and any other parts as non-empty text. One shape for every question, so
/// a reader of a request finds the same keys in each and a part that
/// depends on the alert can be named by `part_when`.
fn check_instructions(id: &str, question: &Question) -> Result<(), String> {
    let (Question::Noul { instructions, .. }
    | Question::Choice { instructions, .. }
    | Question::Score { instructions, .. }) = question;
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

/// `owner` and `duplicate_of` are asked over options signalman supplies per
/// alert: `options_from: request`, with the no-match option the one static
/// option, described.
fn supplied_choice(rubric: &Rubric, id: &str, key: &str) -> Result<(), String> {
    let rq: &RubricQuestion = &rubric.questions[id];
    if rq.options_from != Some(OptionsFrom::Request) {
        return Err(format!(
            "`{id}` must say `options_from: request`: signalman supplies its options per alert"
        ));
    }
    match &rq.question {
        Question::Choice { criteria, .. }
            if criteria.len() == 1 && criteria.get(key).is_some_and(non_empty_text) =>
        {
            Ok(())
        }
        _ => Err(format!(
            "`{id}`'s one static option is `{key}`, described: the no-match option, sent after the options supplied per alert"
        )),
    }
}

/// The routing policy from the rubric's gates. Each gate must have the one
/// shape `decide` reads; a gate left out keeps the built-in value.
fn policy_from(rubric: &Rubric, levels: &[Value]) -> Result<Policy, String> {
    let mut policy = Policy::default();
    for (id, gate) in &rubric.policy.gates {
        let refuse = |reason: &str| format!("policy.{id}: {reason}");
        let only = |allowed: &[&str]| -> Result<(), String> {
            let used = [
                ("threshold", gate.threshold.is_some()),
                ("confidence", gate.confidence.is_some()),
                ("bands", !gate.bands.is_empty()),
                ("fallback", gate.fallback.is_some()),
                ("level_at_least", gate.level_at_least.is_some()),
                ("strict", gate.strict),
            ];
            match used
                .iter()
                .find(|(name, on)| *on && !allowed.contains(name))
            {
                Some((name, _)) => Err(format!(
                    "policy.{id}: `{name}` is not read here; this gate takes {}",
                    allowed
                        .iter()
                        .map(|a| format!("`{a}`"))
                        .collect::<Vec<_>>()
                        .join(", ")
                )),
                None => Ok(()),
            }
        };
        match id.as_str() {
            ACTIONABLE => {
                only(&["threshold"])?;
                if let Some(threshold) = gate.threshold {
                    policy.suppress_below = threshold;
                }
            }
            OWNER => {
                only(&["confidence", "bands", "fallback"])?;
                owner_bars(gate, &mut policy).map_err(|reason| refuse(&reason))?;
                if gate.fallback.as_deref().is_some_and(|f| f != NONE_OF_THESE) {
                    return Err(refuse(
                        "the fallback is `none_of_these`: below the last bar a person triages",
                    ));
                }
            }
            IMPACT => {
                only(&["level_at_least"])?;
                if let Some(level) = &gate.level_at_least {
                    let index = level.resolve(levels).ok_or_else(|| {
                        format!("policy.{id}: `level_at_least` names no impact level")
                    })?;
                    policy.page_at = Impact::from_level(index);
                }
            }
            DUPLICATE_OF => {
                only(&["confidence", "fallback"])?;
                if let Some(confidence) = gate.confidence {
                    policy.attach_confidence = confidence;
                }
                if gate.fallback.as_deref().is_some_and(|f| f != NO_DUPLICATE) {
                    return Err(refuse(
                        "the fallback is `none`: below the bar the alert is not attached",
                    ));
                }
            }
            CAUSED_BY_CHANGE => {
                only(&["threshold", "strict"])?;
                if gate.threshold.is_some() && !gate.strict {
                    return Err(refuse(
                        "the change is flagged above the threshold, not at it: say `strict: true`",
                    ));
                }
                if let Some(threshold) = gate.threshold {
                    policy.flag_change_above = threshold;
                }
            }
            // `Rubric::parse` refuses a gate for a question the rubric does
            // not have, and the questions are checked above.
            _ => return Err(refuse("no question signalman routes by has this id")),
        }
    }
    policy.validate()?;
    Ok(policy)
}

/// The owner gate: `bands` named `route` and then, optionally, `confirm`
/// (route automatically, ask the team to confirm), or one `confidence` bar
/// that is both.
fn owner_bars(gate: &Gate, policy: &mut Policy) -> Result<(), String> {
    match (gate.confidence, gate.bands.as_slice()) {
        (Some(bar), []) => {
            policy.auto_route_confidence = bar;
            policy.human_below_confidence = bar;
        }
        (None, [route]) if route.verdict == ROUTE => {
            policy.auto_route_confidence = route.at_least;
            policy.human_below_confidence = route.at_least;
        }
        (None, [route, confirm]) if route.verdict == ROUTE && confirm.verdict == CONFIRM => {
            policy.auto_route_confidence = route.at_least;
            policy.human_below_confidence = confirm.at_least;
        }
        (None, []) => {}
        _ => {
            return Err(format!(
                "the owner's bands are `{ROUTE}` and then, optionally, `{CONFIRM}`, highest first"
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn the_builtin_rubric_is_valid_and_its_policy_is_the_default() {
        let rubric = TriageRubric::parse(BUILTIN, "built-in").unwrap();
        assert_eq!(rubric.id(), "signalman-triage");
        let ids: Vec<&str> = rubric.jud().questions.keys().map(String::as_str).collect();
        assert_eq!(
            ids,
            [OWNER, IMPACT, ACTIONABLE, DUPLICATE_OF, CAUSED_BY_CHANGE]
        );
        assert_eq!(TriageRubric::builtin().jud(), rubric.jud());
        assert_eq!(TriageRubric::builtin().source(), "the built-in rubric");
        assert!(rubric.fingerprint().starts_with("sha256:"));
        // The thresholds the rubric writes are the ones `Policy::default`
        // documents, so a deployment without a rubric routes as before.
        assert_eq!(rubric.policy(), &Policy::default());
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
                "    when: alert.recent_changes\n",
                "    when: alert.recent_changes\n  caused_by_deploy:\n    type: noul\n    instructions: {question: \"Was it the deploy?\"}\n",
                "`caused_by_deploy` is not a question signalman asks",
            ),
            (
                "      question: Which team should own the first response to `alert`?\n",
                "",
                "`owner.instructions.question` must be non-empty text",
            ),
            (
                // Teams written into the rubric instead of supplied per alert.
                "      none_of_these: Not clearly attributable to any listed team from the information given\n    options_from: request\n",
                "      platform: Kubernetes and CI\n      none_of_these: Not clearly attributable to any listed team from the information given\n",
                "`owner` must say `options_from: request`",
            ),
            (
                "      none: \"`alert` is a new, separate problem not covered by any open incident\"\n",
                "      none: \"`alert` is new\"\n      other: another\n",
                "`duplicate_of`'s one static option is `none`",
            ),
            (
                "    when: alert.open_incidents\n",
                "",
                "must keep `when: alert.open_incidents`",
            ),
            (
                "    when: alert.recent_changes\n",
                "    when: alert.labels\n",
                "must keep `when: alert.recent_changes`",
            ),
            (
                "    part_when:\n      catalog: alert.component\n",
                "    when: alert.component\n",
                "`owner` is asked for every alert, so it takes no `when`",
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
        // Its gate goes too: a threshold is a Noul's, and the format refuses
        // it on a Score before signalman reads the question.
        let text = BUILTIN
            .replacen(
                "  caused_by_change:\n    type: noul\n",
                "  caused_by_change:\n    type: score\n    criteria: [no, yes]\n",
                1,
            )
            .replacen(
                "  caused_by_change:\n    threshold: 0.65\n    strict: true\n    note: flag the change above it\n",
                "",
                1,
            );
        let err = TriageRubric::parse(&text, "edited").unwrap_err();
        assert!(
            err.contains("`caused_by_change` is a score, and signalman reads it as a noul"),
            "{err}"
        );
    }

    #[test]
    fn the_policy_is_read_from_the_gates() {
        let rubric = edited("    threshold: 0.25\n", "    threshold: 0.3\n").unwrap();
        assert!((rubric.policy().suppress_below - 0.3).abs() < f64::EPSILON);
        let rubric = edited("    level_at_least: 2\n", "    level_at_least: 3\n").unwrap();
        assert_eq!(rubric.policy().page_at, Impact::Outage);
        // One owner band: route automatically or a person triages.
        let rubric = edited("      - {at_least: 0.40, verdict: confirm}\n", "").unwrap();
        assert!((rubric.policy().human_below_confidence - 0.70).abs() < f64::EPSILON);
        // A gate left out keeps the built-in value.
        let start = BUILTIN.find("  caused_by_change:\n    threshold").unwrap();
        let end = start
            + BUILTIN[start..]
                .find("note: flag the change above it\n")
                .unwrap()
            + "note: flag the change above it\n".len();
        let without = BUILTIN.replacen(&BUILTIN[start..end], "", 1);
        let rubric = TriageRubric::parse(&without, "edited").unwrap();
        assert!((rubric.policy().flag_change_above - 0.65).abs() < f64::EPSILON);
    }

    #[test]
    fn a_gate_decide_cannot_honour_is_refused() {
        let cases = [
            (
                "    threshold: 0.25\n",
                "    threshold: 0.25\n    strict: true\n",
                "`strict` is not read here",
            ),
            (
                "    threshold: 0.65\n    strict: true\n",
                "    threshold: 0.65\n",
                "say `strict: true`",
            ),
            (
                "      - {at_least: 0.40, verdict: confirm}\n",
                "      - {at_least: 0.40, verdict: review}\n",
                "`route` and then, optionally, `confirm`",
            ),
            (
                "    level_at_least: 2\n",
                "    level_at_least: 2\n    confidence: 0.5\n",
                "`confidence` is not read here",
            ),
            (
                "    confidence: 0.75\n    fallback: none\n",
                "    bands: [{at_least: 0.75, verdict: attach}]\n",
                "`bands` is not read here",
            ),
        ];
        for (from, to, needle) in cases {
            let err = edited(from, to).unwrap_err();
            assert!(err.contains(needle), "{needle}: {err}");
        }
    }

    #[test]
    fn a_part_is_left_out_of_every_question() {
        let off = TriageRubric::builtin().without_part("rule").unwrap();
        for (id, rq) in &off.jud().questions {
            let json = serde_json::to_value(&rq.question).unwrap();
            assert!(json["instructions"].get("rule").is_none(), "{id}");
            assert!(json["instructions"]["question"].is_string(), "{id}");
        }
        assert_ne!(off.fingerprint(), TriageRubric::builtin().fingerprint());
        assert!(off.source().ends_with("without `rule`"), "{}", off.source());
        assert!(TriageRubric::builtin().without_part("question").is_err());
        // Leaving out a part that has a `part_when` drops the condition too.
        let no_catalog = TriageRubric::builtin().without_part("catalog").unwrap();
        assert!(no_catalog.jud().questions[OWNER].part_when.is_empty());
    }

    #[test]
    fn a_format_error_names_the_source_and_the_line() {
        let err = TriageRubric::parse("jud: 1\nkind: rubric\nid: x\nquestions: [\n", "my.jud")
            .unwrap_err();
        assert!(err.starts_with("my.jud: "), "{err}");
        assert!(err.contains("line"), "{err}");
    }
}
