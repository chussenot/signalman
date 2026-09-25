//! Routing policy. Pure code over typed answers, so it is unit-testable
//! without the model and tunable without re-running inference.

use serde::{Deserialize, Serialize};

use super::questions::{Impact, NO_DUPLICATE, NONE_OF_THESE, TriageAnswers};
use super::{Owner, OwnerCandidate};

/// Thresholds. Start conservative, then tune on your own alert history and
/// pin the model version you tuned against. Deserialises from the `[policy]`
/// table of the configuration file; absent fields keep their defaults.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Policy {
    /// Below this `actionable` probability the alert is suppressed.
    pub suppress_below: f64,
    /// Minimum dedup confidence to attach to an existing incident.
    pub attach_confidence: f64,
    /// Owner confidence at or above which routing is automatic.
    pub auto_route_confidence: f64,
    /// Owner confidence below which a human triages instead.
    pub human_below_confidence: f64,
    /// Impact at or above which the owner is paged rather than ticketed.
    pub page_at: Impact,
    /// `caused_by_change` probability above which the change is flagged.
    pub flag_change_above: f64,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            suppress_below: 0.25,
            attach_confidence: 0.75,
            auto_route_confidence: 0.70,
            human_below_confidence: 0.40,
            page_at: Impact::Major,
            flag_change_above: 0.65,
        }
    }
}

impl Policy {
    /// Every probability threshold in `0..=1`; the human threshold at or
    /// below the automatic one, or no confidence could route automatically.
    pub fn validate(&self) -> Result<(), String> {
        let unit = [
            ("suppress_below", self.suppress_below),
            ("attach_confidence", self.attach_confidence),
            ("auto_route_confidence", self.auto_route_confidence),
            ("human_below_confidence", self.human_below_confidence),
            ("flag_change_above", self.flag_change_above),
        ];
        for (name, v) in unit {
            if !(0.0..=1.0).contains(&v) {
                return Err(format!("policy.{name} = {v} is outside 0..=1"));
            }
        }
        if self.human_below_confidence > self.auto_route_confidence {
            return Err(format!(
                "policy.human_below_confidence ({}) must not exceed policy.auto_route_confidence ({})",
                self.human_below_confidence, self.auto_route_confidence
            ));
        }
        Ok(())
    }
}

/// What to do with the alert.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum Decision {
    /// Log and drop: the model is confident nobody needs to act.
    Suppress {
        /// Probability the alert was actionable.
        actionable: f64,
    },
    /// Add to an existing incident instead of opening a new one.
    AttachToIncident {
        /// The incident id.
        incident_id: String,
        /// Dedup confidence.
        confidence: f64,
    },
    /// Wake the owning team now.
    Page {
        /// Owning team.
        owner: Owner,
        /// Impact level.
        impact: Impact,
        /// Owner confidence was below the auto threshold; the responder
        /// should confirm ownership.
        confirm_owner: bool,
        /// A listed recent change is a likely cause.
        suspected_change: bool,
    },
    /// Open a ticket for the owning team; no page.
    Ticket {
        /// Owning team.
        owner: Owner,
        /// Impact level.
        impact: Impact,
        /// Owner confidence was middling; ask the team to confirm ownership.
        confirm_owner: bool,
        /// A listed recent change is a likely cause.
        suspected_change: bool,
    },
    /// The model is unsure who owns it: a person triages.
    HumanTriage {
        /// Best guess, for the triager's convenience.
        best_guess: Owner,
        /// Owner confidence.
        confidence: f64,
        /// Impact level, which still decides urgency of the triage queue.
        impact: Impact,
    },
}

impl Decision {
    /// The owner the decision addresses, if any.
    pub fn owner(&self) -> Option<&Owner> {
        match self {
            Self::Page { owner, .. } | Self::Ticket { owner, .. } => Some(owner),
            Self::HumanTriage { best_guess, .. } => Some(best_guess),
            Self::Suppress { .. } | Self::AttachToIncident { .. } => None,
        }
    }
}

impl Impact {
    /// The lowercase wire and configuration name.
    pub fn key(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Minor => "minor",
            Self::Major => "major",
            Self::Outage => "outage",
        }
    }
}

impl Serialize for Impact {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.key())
    }
}

impl<'de> Deserialize<'de> for Impact {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(d)?;
        match raw.to_ascii_lowercase().as_str() {
            "none" => Ok(Self::None),
            "minor" => Ok(Self::Minor),
            "major" => Ok(Self::Major),
            "outage" => Ok(Self::Outage),
            other => Err(serde::de::Error::unknown_variant(
                other,
                &["none", "minor", "major", "outage"],
            )),
        }
    }
}

/// Apply the policy. Order encodes priority: suppression first, then dedup,
/// then routing by owner confidence, with impact deciding page vs ticket.
pub fn decide(answers: &TriageAnswers, policy: &Policy) -> Decision {
    let actionable = answers.actionable.yes.value();
    if actionable < policy.suppress_below {
        return Decision::Suppress { actionable };
    }

    if let Some(dup) = &answers.duplicate_of
        && dup.chosen != NO_DUPLICATE
        && dup.confidence.at_least(policy.attach_confidence)
    {
        return Decision::AttachToIncident {
            incident_id: dup.chosen.clone(),
            confidence: dup.confidence.value(),
        };
    }

    let impact = Impact::from_level(answers.impact.nearest_level());
    let owner_conf = answers.owner.confidence.value();
    let suspected_change = answers
        .caused_by_change
        .is_some_and(|n| n.yes.value() > policy.flag_change_above);

    let chosen = answers.candidates.get(&answers.owner.chosen);

    // "None of these", an option outside the candidate set, or low confidence
    // all mean a person decides.
    let owner = match chosen {
        Some(c) if c.key != NONE_OF_THESE && owner_conf >= policy.human_below_confidence => {
            Owner::from(c)
        }
        _ => {
            return Decision::HumanTriage {
                best_guess: Owner::from(best_named(answers)),
                confidence: owner_conf,
                impact,
            };
        }
    };

    let confirm_owner = owner_conf < policy.auto_route_confidence;
    if impact >= policy.page_at {
        Decision::Page {
            owner,
            impact,
            confirm_owner,
            suspected_change,
        }
    } else {
        Decision::Ticket {
            owner,
            impact,
            confirm_owner,
            suspected_change,
        }
    }
}

/// Highest-probability candidate other than the no-match option.
fn best_named(answers: &TriageAnswers) -> &OwnerCandidate {
    let named: Vec<&OwnerCandidate> = answers
        .candidates
        .iter()
        .filter(|c| c.key != NONE_OF_THESE)
        .collect();
    named
        .iter()
        .copied()
        .max_by(|a, b| {
            answers
                .owner
                .probability_of(&a.key)
                .partial_cmp(&answers.owner.probability_of(&b.key))
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .or_else(|| answers.candidates.iter().next())
        .unwrap_or_else(|| unreachable!("candidate set always has the no-match option"))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::triage::{Alert, OpenIncident, TriageQuestions};
    use serde_json::json;
    use std::collections::BTreeMap;

    /// Build typed answers by round-tripping a fake API response through the
    /// real handles, so the test exercises the same path as production.
    fn answers(open_incident: bool, raw: &serde_json::Value) -> TriageAnswers {
        let mut alert = Alert {
            source: "prometheus".into(),
            title: "t".into(),
            description: "d".into(),
            labels: BTreeMap::new(),
            runbook: None,
            recent_changes: vec!["deploy".into()],
            open_incidents: vec![],
            component: None,
            related_alerts: vec![],
        };
        if open_incident {
            alert.open_incidents.push(OpenIncident {
                id: "INC-1".into(),
                summary: "s".into(),
            });
        }
        let q = TriageQuestions::for_alert(&alert).unwrap();
        let response: crate::Response = serde_json::from_value(json!({
            "model": "jev-1.13.0", "answers": raw, "usage": {"input_tokens": 1, "output_tokens": 1}
        }))
        .unwrap();
        q.read(&response).unwrap()
    }

    fn owner(team: &str, conf: f64) -> serde_json::Value {
        let mut probs = serde_json::Map::new();
        probs.insert(team.into(), json!(conf));
        probs.insert("none_of_these".into(), json!(1.0 - conf));
        json!({ "type": "choice", "choice": team, "probabilities": probs, "confidence": conf })
    }

    fn impact(level: f64) -> serde_json::Value {
        // The legend is the levels as sent, as TypeSafe echoes them.
        let legend: serde_json::Map<String, serde_json::Value> = crate::triage::Impact::LEVELS
            .iter()
            .enumerate()
            .map(|(i, l)| (i.to_string(), json!(l)))
            .collect();
        json!({ "type": "score", "score": level,
                "legend": legend,
                "probabilities": {"0":0.0,"1":0.0,"2":0.0,"3":0.0}, "confidence": 0.9 })
    }

    fn noul(p: f64) -> serde_json::Value {
        json!({ "type": "noul", "noul": p })
    }

    #[test]
    fn noise_is_suppressed_before_anything_else() {
        let a = answers(
            false,
            &json!({
                "owner": owner("platform", 0.99), "impact": impact(3.0),
                "actionable": noul(0.1), "caused_by_change": noul(0.9)
            }),
        );
        assert!(matches!(
            decide(&a, &Policy::default()),
            Decision::Suppress { .. }
        ));
    }

    #[test]
    fn confident_duplicate_attaches() {
        let a = answers(
            true,
            &json!({
                "owner": owner("database", 0.9), "impact": impact(2.0), "actionable": noul(0.9),
                "caused_by_change": noul(0.1),
                "duplicate_of": { "type": "choice", "choice": "INC-1",
                                  "probabilities": {"INC-1": 0.9, "none": 0.1}, "confidence": 0.8 }
            }),
        );
        assert_eq!(
            decide(&a, &Policy::default()),
            Decision::AttachToIncident {
                incident_id: "INC-1".into(),
                confidence: 0.8
            }
        );
    }

    #[test]
    fn unsure_duplicate_falls_through_to_routing() {
        let a = answers(
            true,
            &json!({
                "owner": owner("database", 0.9), "impact": impact(2.0), "actionable": noul(0.9),
                "caused_by_change": noul(0.1),
                "duplicate_of": { "type": "choice", "choice": "INC-1",
                                  "probabilities": {"INC-1": 0.55, "none": 0.45}, "confidence": 0.1 }
            }),
        );
        assert!(
            matches!(decide(&a, &Policy::default()), Decision::Page { owner: Owner { ref key, .. }, .. } if key == "database")
        );
    }

    #[test]
    fn impact_decides_page_versus_ticket_and_confidence_decides_confirmation() {
        let page = answers(
            false,
            &json!({
                "owner": owner("network", 0.95), "impact": impact(2.6), "actionable": noul(0.9),
                "caused_by_change": noul(0.8)
            }),
        );
        match decide(&page, &Policy::default()) {
            Decision::Page {
                owner,
                impact,
                confirm_owner,
                suspected_change,
            } => {
                assert_eq!(owner.key, "network");
                assert_eq!(impact, Impact::Outage);
                assert!(!confirm_owner);
                assert!(suspected_change);
            }
            other => panic!("{other:?}"),
        }

        let ticket = answers(
            false,
            &json!({
                "owner": owner("application", 0.55), "impact": impact(1.2), "actionable": noul(0.9),
                "caused_by_change": noul(0.2)
            }),
        );
        match decide(&ticket, &Policy::default()) {
            Decision::Ticket {
                owner,
                impact,
                confirm_owner,
                suspected_change,
            } => {
                assert_eq!(owner.key, "application");
                assert_eq!(impact, Impact::Minor);
                assert!(confirm_owner);
                assert!(!suspected_change);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn low_owner_confidence_or_none_of_these_goes_to_a_human() {
        let low = answers(
            false,
            &json!({
                "owner": owner("security", 0.3), "impact": impact(2.0), "actionable": noul(0.9),
                "caused_by_change": noul(0.2)
            }),
        );
        assert!(matches!(
            decide(&low, &Policy::default()),
            Decision::HumanTriage { best_guess: Owner { ref key, .. }, .. } if key == "security"
        ));

        let none = answers(
            false,
            &json!({
                "owner": owner("none_of_these", 0.9), "impact": impact(0.4), "actionable": noul(0.9),
                "caused_by_change": noul(0.2)
            }),
        );
        assert!(matches!(
            decide(&none, &Policy::default()),
            Decision::HumanTriage { .. }
        ));
    }

    #[test]
    fn decision_serialises_with_a_tag() {
        let d = Decision::Ticket {
            owner: Owner {
                key: "platform".into(),
                label: "platform".into(),
                entity_ref: None,
            },
            impact: Impact::Minor,
            confirm_owner: false,
            suspected_change: false,
        };
        let v = serde_json::to_value(&d).unwrap();
        assert_eq!(v["action"], "ticket");
        assert_eq!(v["owner"]["key"], "platform");
        assert_eq!(v["impact"], "minor");
    }
}
