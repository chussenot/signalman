//! The fan-out: every judgment the routing policy might need, asked in one
//! request. Questions that turn out irrelevant are simply not read.

use serde_json::{Value, json};

use super::Alert;
use crate::answer::{Choice, Noul, Response, Score};
use crate::question::{Handle, NoulCriteria, Questions};
use crate::{Result, options};

options! {
    /// Which team owns first response.
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
    /// Who should own first response.
    pub owner: Handle<Choice<Team>>,
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
    /// Build the question set for an alert. Questions reference the state by
    /// backticked path, as the docs recommend.
    pub fn for_alert(alert: &Alert) -> Result<Self> {
        let mut q = Questions::new();

        let owner = q.choice::<Team>(
            "owner",
            json!({
                "question": "Which team should own the first response to `alert`?",
                "guidance": "Decide from the failing component in `alert.title`, `alert.description` and `alert.labels`, not from who is mentioned. Use `alert.runbook` when present.",
            }),
        )?;

        let impact = q.score(
            "impact",
            "What is the current user-facing impact described by `alert`?",
            Impact::LEVELS,
        )?;

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
    /// Owning team with distribution and confidence.
    pub owner: Choice<Team>,
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
    use crate::triage::OpenIncident;

    fn alert() -> Alert {
        Alert {
            source: "prometheus".into(),
            title: "KubePodCrashLooping".into(),
            description: "checkout-api restarting".into(),
            labels: BTreeMap::new(),
            runbook: None,
            recent_changes: vec![],
            open_incidents: vec![],
        }
    }
    use std::collections::BTreeMap;

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
    fn team_keys_round_trip() {
        for t in Team::ALL {
            assert_eq!(Team::from_key(t.key()), Some(*t));
        }
        assert_eq!(Team::from_key("none_of_these"), Some(Team::Unclear));
    }

    #[test]
    fn impact_levels_match_the_enum() {
        assert_eq!(Impact::LEVELS.len(), 4);
        assert_eq!(Impact::from_level(0), Impact::None);
        assert_eq!(Impact::from_level(3), Impact::Outage);
        assert_eq!(Impact::from_level(99), Impact::Outage);
    }
}
