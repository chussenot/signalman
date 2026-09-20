//! The fan-out: every judgment the routing policy might need, asked in one
//! request. Questions that turn out irrelevant are simply not read.

use serde_json::{Value, json};

use super::{Alert, OwnerCandidates};
use crate::answer::{Choice, Noul, Response, Score};
use crate::question::{Handle, NoulCriteria, Questions};
use crate::{Result, options};

options! {
    /// The static team list, used when no software catalog is configured.
    /// Adapt it to your organisation, or configure Backstage and let the
    /// catalog supply the groups.
    pub enum Team {
        /// Kubernetes, CI/CD, IDP, cluster add-ons.
        Platform = "platform" => "Kubernetes, cluster add-ons, CI/CD, internal developer platform, cloud accounts and quotas",
        /// Databases and storage.
        Database = "database" => "PostgreSQL, MySQL, Redis, message queues, backups, storage volumes",
        /// Network and edge.
        Network = "network" => "DNS, load balancers, ingress, CDN, VPN, inter-region connectivity",
        /// Product services.
        Application = "application" => "A product service's own code or configuration: errors, latency, business logic",
        /// Security.
        Security = "security" => "Suspicious access, secrets exposure, vulnerability findings, policy violations",
        /// Observability tooling.
        Observability = "observability" => "Monitoring, logging or tracing pipeline itself is broken or lagging",
        /// None of the above.
        Unclear = "none_of_these" => "Not clearly attributable to any listed team from the information given",
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
    /// Build the question set with the static team list as owner candidates.
    pub fn for_alert(alert: &Alert) -> Result<Self> {
        Self::for_alert_with(alert, OwnerCandidates::from_teams())
    }

    /// Build the question set with explicit owner candidates (for example
    /// groups from the software catalog). Questions reference the state by
    /// backticked path, as the docs recommend.
    pub fn for_alert_with(alert: &Alert, candidates: OwnerCandidates) -> Result<Self> {
        let mut q = Questions::new();

        let mut owner_instructions = json!({
            "question": "Which team should own the first response to `alert`?",
            "guidance": "Decide from the failing component in `alert.title`, `alert.description` and `alert.labels`, not from who is mentioned. Use `alert.runbook` when present.",
        });
        if alert.component.is_some() {
            owner_instructions["catalog"] = json!(
                "`alert.component` is the software catalog's record of the alerting component. `alert.component.owner` is its registered owner; prefer that team unless the alert clearly concerns one of `alert.component.depends_on` or another component instead. `alert.component.dependents` shows what breaks downstream."
            );
        }
        let owner = q.dynamic_choice(
            "owner",
            owner_instructions,
            candidates
                .iter()
                .map(|c| (c.key.clone(), Some(c.description.clone()))),
        )?;

        let impact_instructions = if alert.related_alerts.is_empty() {
            json!("What is the current user-facing impact described by `alert`?")
        } else {
            json!({
                "question": "What is the current user-facing impact described by `alert`?",
                "context": "`alert.related_alerts` lists other alerts firing in the same window with their age in minutes. Several on the same component, or on components in `alert.component.dependents`, indicate broader impact than `alert` alone shows; unrelated ones do not raise it.",
            })
        };
        let impact = q.score("impact", impact_instructions, Impact::LEVELS)?;

        let actionable = q.noul(
            "actionable",
            "Does `alert` describe a condition that a person must act on now, rather than informational or self-resolving noise?",
            Some(NoulCriteria::new(
                "A component is failing, at risk, or violating a policy, and the situation will not resolve on its own",
                "Informational, a test alert, already recovered, or a threshold blip with no consequence",
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
                    Some(
                        "`alert` is a new, separate problem not covered by any open incident"
                            .to_owned(),
                    ),
                )));
            Some(q.dynamic_choice(
                "duplicate_of",
                "Which entry in `alert.open_incidents` is the same underlying problem as `alert`? Choose `none` if it is a distinct problem.",
                options,
            )?)
        };

        let caused_by_change = if alert.recent_changes.is_empty() {
            None
        } else {
            Some(q.noul(
                "caused_by_change",
                "Is one of `alert.recent_changes` a plausible direct cause of `alert`, given timing and the component involved?",
                None,
            )?)
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
    use crate::Options;
    use crate::triage::{OpenIncident, OwnerCandidate};
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
    fn static_owner_candidates_mirror_the_team_enum() {
        let q = TriageQuestions::for_alert(&alert()).unwrap();
        let json = serde_json::to_value(&q.questions).unwrap();
        let criteria = json["owner"]["criteria"].as_object().unwrap();
        for t in Team::ALL {
            assert!(criteria.contains_key(t.key()), "{}", t.key());
        }
        assert_eq!(criteria.len(), Team::ALL.len());
        assert!(json["owner"]["instructions"].get("catalog").is_none());
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
        assert_eq!(Impact::from_level(0), Impact::None);
        assert_eq!(Impact::from_level(3), Impact::Outage);
        assert_eq!(Impact::from_level(99), Impact::Outage);
    }
}
