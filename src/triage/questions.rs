//! The fan-out: every judgment the routing policy might need, asked in one
//! request. Questions that turn out irrelevant are simply not read.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{Alert, OwnerCandidates};
use crate::Result;
use crate::answer::{Choice, Noul, Response, Score};
use crate::question::{Handle, NoulCriteria, Questions};

/// The words of every question: instructions, guidance and criteria. The
/// *set* of questions and their primitive types are code, because the policy
/// consumes them; the text is data, because it is what a team tunes to its
/// own vocabulary and alert sources. Overridden by `[triage.text]` in the
/// configuration file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Texts {
    /// Owner question.
    pub owner_question: String,
    /// Owner guidance, always sent.
    pub owner_guidance: String,
    /// Owner guidance added when `alert.component` is present.
    pub owner_catalog_guidance: String,
    /// Impact question.
    pub impact_question: String,
    /// Impact context added when `alert.related_alerts` is non-empty.
    pub impact_related_context: String,
    /// The impact levels, lowest first. Exactly as many entries as
    /// `Impact::LEVELS`: the policy compares against [`Impact`] variants.
    pub impact_levels: Vec<String>,
    /// Actionable question.
    pub actionable_question: String,
    /// What a yes means for actionable.
    pub actionable_yes: String,
    /// What a no means for actionable.
    pub actionable_no: String,
    /// Duplicate question.
    pub duplicate_question: String,
    /// Rubric of the `none` dedup option.
    pub duplicate_none: String,
    /// Caused-by-change question.
    pub change_question: String,
}

impl Default for Texts {
    fn default() -> Self {
        Self {
            owner_question: "Which team should own the first response to `alert`?".into(),
            owner_guidance: "Decide from the failing component in `alert.title`, `alert.description` and `alert.labels`, not from who is mentioned. Use `alert.runbook` when present.".into(),
            owner_catalog_guidance: "`alert.component` is the software catalog's record of the alerting component. `alert.component.owner` is its registered owner; prefer that team unless the alert clearly concerns one of `alert.component.depends_on` or another component instead. `alert.component.dependents` shows what breaks downstream.".into(),
            impact_question: "What is the current user-facing impact described by `alert`?".into(),
            impact_related_context: "`alert.related_alerts` lists other alerts firing in the same window with their age in minutes. Several on the same component, or on components in `alert.component.dependents`, indicate broader impact than `alert` alone shows; unrelated ones do not raise it.".into(),
            impact_levels: Impact::LEVELS.map(String::from).to_vec(),
            actionable_question: "Does `alert` describe a condition that a person must act on now, rather than informational or self-resolving noise?".into(),
            actionable_yes: "A component is failing, at risk, or violating a policy, and the situation will not resolve on its own".into(),
            actionable_no: "Informational, a test alert, already recovered, or a threshold blip with no consequence".into(),
            duplicate_question: "Which entry in `alert.open_incidents` is the same underlying problem as `alert`? Choose `none` if it is a distinct problem.".into(),
            duplicate_none: "`alert` is a new, separate problem not covered by any open incident".into(),
            change_question: "Is one of `alert.recent_changes` a plausible direct cause of `alert`, given timing and the component involved?".into(),
        }
    }
}

impl Texts {
    /// The level count is fixed by [`Impact`]; every string must be non-empty,
    /// since an empty instruction is a question with no meaning.
    pub fn validate(&self) -> Result<(), String> {
        if self.impact_levels.len() != Impact::LEVELS.len() {
            return Err(format!(
                "triage.text.impact_levels must have exactly {} entries (got {})",
                Impact::LEVELS.len(),
                self.impact_levels.len()
            ));
        }
        let fields = [
            ("owner_question", &self.owner_question),
            ("owner_guidance", &self.owner_guidance),
            ("owner_catalog_guidance", &self.owner_catalog_guidance),
            ("impact_question", &self.impact_question),
            ("impact_related_context", &self.impact_related_context),
            ("actionable_question", &self.actionable_question),
            ("actionable_yes", &self.actionable_yes),
            ("actionable_no", &self.actionable_no),
            ("duplicate_question", &self.duplicate_question),
            ("duplicate_none", &self.duplicate_none),
            ("change_question", &self.change_question),
        ];
        for (name, v) in fields {
            if v.trim().is_empty() {
                return Err(format!("triage.text.{name} must not be empty"));
            }
        }
        if self.impact_levels.iter().any(|l| l.trim().is_empty()) {
            return Err("triage.text.impact_levels entries must not be empty".into());
        }
        Ok(())
    }
}

/// Key of the owner question's no-match option.
pub const NONE_OF_THESE: &str = "none_of_these";

/// User-facing impact levels, lowest first. Order matters: the Score's
/// numeric value is a position on this list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Impact {
    /// Internal only.
    None,
    /// Some users, degraded.
    Minor,
    /// Most users, degraded.
    Major,
    /// Everyone, down.
    Outage,
}

impl Impact {
    /// The rubric sent to the model, in level order.
    pub const LEVELS: [&'static str; 4] = [
        "No user-facing impact: internal, informational, or affects only non-production",
        "Degraded experience for a small subset of users or one non-critical feature",
        "Degraded or failing for most users, or a critical feature is unavailable",
        "Full outage: the product is unavailable or data integrity is at risk for everyone",
    ];

    /// Map a level index to the enum.
    pub fn from_level(level: usize) -> Self {
        match level {
            0 => Self::None,
            1 => Self::Minor,
            2 => Self::Major,
            _ => Self::Outage,
        }
    }
}

/// Key used for "no duplicate" in the dedup Choice.
pub const NO_DUPLICATE: &str = "none";

/// Handles for every question in the triage request.
#[derive(Debug, Clone)]
pub struct TriageQuestions {
    /// The wire questions.
    pub questions: Questions,
    /// The owner option set, to map the chosen key back to a candidate.
    pub candidates: OwnerCandidates,
    /// Who should own first response.
    pub owner: Handle<Choice<String>>,
    /// User-facing impact.
    pub impact: Handle<Score>,
    /// Does a human need to act now?
    pub actionable: Handle<Noul>,
    /// Which open incident this belongs to, if any. Only asked when there are
    /// candidates: the model cannot choose an option that is not offered.
    pub duplicate_of: Option<Handle<Choice<String>>>,
    /// Is a listed recent change the likely cause? Only asked when there are
    /// recent changes.
    pub caused_by_change: Option<Handle<Noul>>,
}

impl TriageQuestions {
    /// Build the question set with the built-in fallback teams and default text.
    pub fn for_alert(alert: &Alert) -> Result<Self> {
        Self::for_alert_with(alert, OwnerCandidates::from_teams())
    }

    /// Build with explicit owner candidates (for example groups from the
    /// software catalog) and default text.
    pub fn for_alert_with(alert: &Alert, candidates: OwnerCandidates) -> Result<Self> {
        Self::for_alert_with_texts(alert, candidates, &Texts::default())
    }

    /// Build with explicit owner candidates and text. Questions reference the
    /// state by backticked path, as the docs recommend; which questions are
    /// asked depends on the state, never on the text.
    pub fn for_alert_with_texts(
        alert: &Alert,
        candidates: OwnerCandidates,
        texts: &Texts,
    ) -> Result<Self> {
        let mut q = Questions::new();

        let mut owner_instructions = json!({
            "question": texts.owner_question,
            "guidance": texts.owner_guidance,
        });
        if alert.component.is_some() {
            owner_instructions["catalog"] = json!(texts.owner_catalog_guidance);
        }
        let owner = q.dynamic_choice(
            "owner",
            owner_instructions,
            candidates
                .iter()
                .map(|c| (c.key.clone(), Some(c.description.clone()))),
        )?;

        let impact_instructions = if alert.related_alerts.is_empty() {
            json!(texts.impact_question)
        } else {
            json!({
                "question": texts.impact_question,
                "context": texts.impact_related_context,
            })
        };
        let impact = q.score(
            "impact",
            impact_instructions,
            texts.impact_levels.iter().map(String::as_str),
        )?;

        let actionable = q.noul(
            "actionable",
            texts.actionable_question.as_str(),
            Some(NoulCriteria::new(
                texts.actionable_yes.as_str(),
                texts.actionable_no.as_str(),
            )),
        )?;

        let duplicate_of = if alert.open_incidents.is_empty() {
            None
        } else {
            let options = alert
                .open_incidents
                .iter()
                .map(|i| (i.id.clone(), Some(i.summary.clone())))
                .chain(std::iter::once((
                    NO_DUPLICATE.to_owned(),
                    Some(texts.duplicate_none.clone()),
                )));
            Some(q.dynamic_choice("duplicate_of", texts.duplicate_question.as_str(), options)?)
        };

        let caused_by_change = if alert.recent_changes.is_empty() {
            None
        } else {
            Some(q.noul("caused_by_change", texts.change_question.as_str(), None)?)
        };

        Ok(Self {
            questions: q,
            candidates,
            owner,
            impact,
            actionable,
            duplicate_of,
            caused_by_change,
        })
    }

    /// The `state` sent alongside the questions.
    pub fn state(alert: &Alert) -> Value {
        json!({ "alert": alert })
    }

    /// Read every answer through its handle.
    pub fn read(&self, response: &Response) -> Result<TriageAnswers> {
        Ok(TriageAnswers {
            owner: response.get(&self.owner)?,
            candidates: self.candidates.clone(),
            impact: response.get(&self.impact)?,
            actionable: response.get(&self.actionable)?,
            duplicate_of: self
                .duplicate_of
                .as_ref()
                .map(|h| response.get(h))
                .transpose()?,
            caused_by_change: self
                .caused_by_change
                .as_ref()
                .map(|h| response.get(h))
                .transpose()?,
            model: response.model.clone(),
        })
    }
}

/// Typed answers for one alert.
#[derive(Debug, Clone, PartialEq)]
pub struct TriageAnswers {
    /// Owning team key with distribution and confidence.
    pub owner: Choice<String>,
    /// The option set the owner was chosen from.
    pub candidates: OwnerCandidates,
    /// Impact on the [`Impact::LEVELS`] scale.
    pub impact: Score,
    /// Probability a human must act.
    pub actionable: Noul,
    /// Chosen open incident id or [`NO_DUPLICATE`], when candidates existed.
    pub duplicate_of: Option<Choice<String>>,
    /// Probability a recent change is the cause, when changes were listed.
    pub caused_by_change: Option<Noul>,
    /// Versioned model that answered.
    pub model: String,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::triage::{OpenIncident, OwnerCandidate, default_teams};
    use std::collections::BTreeMap;

    fn alert() -> Alert {
        Alert {
            source: "prometheus".into(),
            title: "KubePodCrashLooping".into(),
            description: "checkout-api restarting".into(),
            labels: BTreeMap::new(),
            runbook: None,
            recent_changes: vec![],
            open_incidents: vec![],
            component: None,
            related_alerts: vec![],
        }
    }

    #[test]
    fn speculative_questions_are_only_asked_when_answerable() {
        let bare = TriageQuestions::for_alert(&alert()).unwrap();
        assert!(bare.duplicate_of.is_none());
        assert!(bare.caused_by_change.is_none());
        assert_eq!(bare.questions.len(), 3);

        let mut rich = alert();
        rich.open_incidents.push(OpenIncident {
            id: "INC-1".into(),
            summary: "checkout down".into(),
        });
        rich.recent_changes.push("deploy checkout-api v2".into());
        let q = TriageQuestions::for_alert(&rich).unwrap();
        assert!(q.duplicate_of.is_some());
        assert!(q.caused_by_change.is_some());
        assert_eq!(q.questions.len(), 5);

        let json = serde_json::to_value(&q.questions).unwrap();
        let dedup = &json["duplicate_of"]["criteria"];
        assert!(dedup.get("INC-1").is_some());
        assert!(dedup.get(NO_DUPLICATE).is_some());
    }

    #[test]
    fn fallback_owner_candidates_are_the_default_teams_plus_no_match() {
        let q = TriageQuestions::for_alert(&alert()).unwrap();
        let json = serde_json::to_value(&q.questions).unwrap();
        let criteria = json["owner"]["criteria"].as_object().unwrap();
        for t in default_teams() {
            assert!(criteria.contains_key(&t.key), "{}", t.key);
        }
        assert_eq!(criteria.len(), default_teams().len() + 1);
        assert!(criteria.contains_key(NONE_OF_THESE));
        assert!(json["owner"]["instructions"].get("catalog").is_none());
    }

    #[test]
    fn text_overrides_change_words_not_questions() {
        let texts = Texts {
            actionable_question: "Must a person act on `alert` now?".into(),
            impact_levels: vec!["l0".into(), "l1".into(), "l2".into(), "l3".into()],
            ..Texts::default()
        };
        let q =
            TriageQuestions::for_alert_with_texts(&alert(), OwnerCandidates::from_teams(), &texts)
                .unwrap();
        let json = serde_json::to_value(&q.questions).unwrap();
        assert_eq!(
            json["actionable"]["instructions"],
            "Must a person act on `alert` now?"
        );
        assert_eq!(json["impact"]["criteria"][2], "l2");
        assert_eq!(q.questions.len(), 3);

        let bad = Texts {
            impact_levels: vec!["only".into()],
            ..Texts::default()
        };
        assert!(bad.validate().unwrap_err().contains("impact_levels"));
        assert!(Texts::default().validate().is_ok());
    }

    #[test]
    fn catalog_candidates_replace_the_static_list_and_add_guidance() {
        let mut a = alert();
        a.component = Some(crate::triage::ComponentContext {
            name: "checkout-api".into(),
            owner: Some("Payments".into()),
            ..Default::default()
        });
        let cands = OwnerCandidates::new(vec![OwnerCandidate {
            key: "payments".into(),
            label: "Payments".into(),
            description: "Owns checkout".into(),
            entity_ref: Some("group:default/payments".into()),
        }]);
        let q = TriageQuestions::for_alert_with(&a, cands).unwrap();
        let json = serde_json::to_value(&q.questions).unwrap();
        let criteria = json["owner"]["criteria"].as_object().unwrap();
        assert_eq!(criteria.len(), 2);
        assert_eq!(criteria["payments"], "Owns checkout");
        assert!(criteria.contains_key(NONE_OF_THESE));
        assert!(
            json["owner"]["instructions"]["catalog"]
                .as_str()
                .unwrap()
                .contains("alert.component.owner")
        );
        let state = TriageQuestions::state(&a);
        assert_eq!(state["alert"]["component"]["owner"], "Payments");
    }

    #[test]
    fn impact_levels_match_the_enum() {
        assert_eq!(Impact::LEVELS.len(), 4);
        assert_eq!(Texts::default().impact_levels.len(), Impact::LEVELS.len());
        assert_eq!(Impact::from_level(0), Impact::None);
        assert_eq!(Impact::from_level(3), Impact::Outage);
        assert_eq!(Impact::from_level(99), Impact::Outage);
    }
}
