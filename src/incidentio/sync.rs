//! The flow: an alert incident.io received → TypeSafe judgments → tags and
//! an incident attachment written back.
//!
//! Follows the webhook docs' rule for keeping systems in sync: the webhook is
//! only a trigger; the alert and the candidate incidents are fetched fresh
//! from the API before judging, so late or reordered deliveries cannot apply
//! a stale decision.

use std::collections::BTreeMap;

use serde::Serialize;

use super::client::Client as IncidentIo;
use super::types::{Alert as IoAlert, Incident};
use crate::Options;
use crate::triage::{
    Alert, Decision, OpenIncident, Policy, TriageAnswers, TriageQuestions, decide,
};

/// Prefix for every tag this integration writes, so they can be filtered in
/// alert routes and told apart from human tags.
pub const TAG_PREFIX: &str = "ai";

/// How many live incidents to offer as dedup candidates. Each is one Choice
/// option (limit 255) and costs input tokens; recent incidents matter most.
pub const DEFAULT_CANDIDATES: usize = 40;

/// Which side effects to apply after deciding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteBack {
    /// Add tags and attach duplicates.
    Apply,
    /// Compute everything, change nothing. For evaluation runs.
    DryRun,
}

/// Runs the flow. Cheap to clone; share one per process.
#[derive(Debug, Clone)]
pub struct Triager {
    /// TypeSafe client.
    pub typesafe: crate::Client,
    /// incident.io client.
    pub incidentio: IncidentIo,
    /// Routing thresholds.
    pub policy: Policy,
    /// Candidate cap for dedup.
    pub max_candidates: usize,
    /// Apply or dry-run.
    pub write_back: WriteBack,
}

/// What happened for one alert.
#[derive(Debug, Clone, Serialize)]
pub struct Outcome {
    /// incident.io alert id.
    pub alert_id: String,
    /// Alert title, for logs.
    pub title: String,
    /// The routing decision.
    pub decision: Decision,
    /// Tags added (or that would be added in a dry run).
    pub tags: Vec<String>,
    /// Incident the alert was attached to, when a confident duplicate.
    pub attached_to: Option<AttachedIncident>,
    /// Versioned model that answered.
    pub model: String,
    /// Dedup candidates offered.
    pub candidates_offered: usize,
    /// Whether side effects were applied.
    pub applied: bool,
}

/// The incident an alert was attached to.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AttachedIncident {
    /// ULID.
    pub id: String,
    /// `INC-123`.
    pub reference: String,
    /// Dedup confidence.
    pub confidence: f64,
}

impl Triager {
    /// Build with default policy and candidate cap.
    pub fn new(typesafe: crate::Client, incidentio: IncidentIo) -> Self {
        Self {
            typesafe,
            incidentio,
            policy: Policy::default(),
            max_candidates: DEFAULT_CANDIDATES,
            write_back: WriteBack::Apply,
        }
    }

    /// Triage an alert by id: fetch, judge, decide, write back.
    pub async fn triage_alert_by_id(&self, alert_id: &str) -> Result<Outcome, FlowError> {
        let alert = self.incidentio.get_alert(alert_id).await?;
        self.triage_alert(alert).await
    }

    /// Triage an already fetched alert. Prefer [`Self::triage_alert_by_id`]
    /// from a webhook so the state is current.
    pub async fn triage_alert(&self, io_alert: IoAlert) -> Result<Outcome, FlowError> {
        let candidates = self
            .incidentio
            .list_open_incidents(self.max_candidates)
            .await?;
        let by_reference: BTreeMap<String, &Incident> = candidates
            .iter()
            .map(|i| (i.reference.clone(), i))
            .collect();

        let alert = to_triage_alert(&io_alert, &candidates);
        let questions = TriageQuestions::for_alert(&alert)?;
        let state = TriageQuestions::state(&alert);
        let response = self
            .typesafe
            .system_one(&state, &questions.questions)
            .await?;
        let answers = questions.read(&response)?;
        let decision = decide(&answers, &self.policy);

        let mut tags = tags_for(&answers, &decision);
        let mut attached_to = None;
        if let Decision::AttachToIncident {
            incident_id: reference,
            confidence,
        } = &decision
        {
            if let Some(inc) = by_reference.get(reference) {
                tags.push(tag("dup", &inc.reference));
                attached_to = Some(AttachedIncident {
                    id: inc.id.clone(),
                    reference: inc.reference.clone(),
                    confidence: *confidence,
                });
            } else {
                tracing::warn!(
                    reference,
                    "dedup chose a reference not in the candidate map"
                );
            }
        }

        let applied = self.write_back == WriteBack::Apply;
        if applied {
            self.incidentio.add_alert_tags(&io_alert.id, &tags).await?;
            if let Some(a) = &attached_to {
                self.incidentio
                    .attach_alert_to_incident(&io_alert.id, &a.id)
                    .await?;
            }
        }

        tracing::info!(
            alert_id = %io_alert.id,
            title = %io_alert.title,
            decision = ?decision,
            attached = ?attached_to.as_ref().map(|a| &a.reference),
            applied,
            model = %response.model,
            "alert triaged"
        );

        Ok(Outcome {
            alert_id: io_alert.id,
            title: io_alert.title,
            decision,
            tags,
            attached_to,
            model: response.model,
            candidates_offered: candidates.len(),
            applied,
        })
    }
}

/// Map an incident.io alert plus live incidents into the triage state.
pub fn to_triage_alert(io: &IoAlert, candidates: &[Incident]) -> Alert {
    let mut labels = io.labels();
    for t in io.tag_names() {
        labels
            .entry("tags".into())
            .and_modify(|v| {
                v.push_str(", ");
                v.push_str(&t);
            })
            .or_insert(t);
    }
    Alert {
        source: format!("incident.io alert source {}", io.alert_source_id),
        title: io.title.clone(),
        description: io.description.clone().unwrap_or_default(),
        labels,
        runbook: None,
        recent_changes: vec![],
        open_incidents: candidates
            .iter()
            .map(|i| OpenIncident {
                id: i.reference.clone(),
                summary: i.candidate_summary(),
            })
            .collect(),
    }
}

/// Tags encoding the judgments and the decision.
pub fn tags_for(answers: &TriageAnswers, decision: &Decision) -> Vec<String> {
    let impact = crate::triage::Impact::from_level(answers.impact.nearest_level());
    let mut tags = vec![
        tag("team", answers.owner.chosen.key()),
        tag("impact", impact_key(impact)),
        tag("action", action_key(decision)),
    ];
    if answers
        .caused_by_change
        .is_some_and(|n| n.yes.value() > 0.65)
    {
        tags.push(tag("suspected-change", ""));
    }
    tags
}

fn tag(kind: &str, value: &str) -> String {
    let v = value.to_ascii_lowercase().replace([' ', '_'], "-");
    if v.is_empty() {
        format!("{TAG_PREFIX}-{kind}")
    } else {
        format!("{TAG_PREFIX}-{kind}-{v}")
    }
}

fn impact_key(i: crate::triage::Impact) -> &'static str {
    use crate::triage::Impact;
    match i {
        Impact::None => "none",
        Impact::Minor => "minor",
        Impact::Major => "major",
        Impact::Outage => "outage",
    }
}

fn action_key(d: &Decision) -> &'static str {
    match d {
        Decision::Suppress { .. } => "suppress",
        Decision::AttachToIncident { .. } => "attach",
        Decision::Page { .. } => "page",
        Decision::Ticket { .. } => "ticket",
        Decision::HumanTriage { .. } => "human-triage",
    }
}

/// Failure anywhere in the flow.
#[derive(Debug, thiserror::Error)]
pub enum FlowError {
    /// incident.io API.
    #[error(transparent)]
    IncidentIo(#[from] super::error::Error),
    /// TypeSafe API or typed-answer layer.
    #[error(transparent)]
    TypeSafe(#[from] crate::Error),
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::incidentio::types::{AlertStatus, IncidentStatus, StatusCategory};

    #[test]
    fn tags_are_lowercase_hyphenated_and_prefixed() {
        assert_eq!(tag("team", "none_of_these"), "ai-team-none-of-these");
        assert_eq!(tag("dup", "INC-4821"), "ai-dup-inc-4821");
        assert_eq!(tag("suspected-change", ""), "ai-suspected-change");
    }

    #[test]
    fn candidates_become_open_incidents_keyed_by_reference() {
        let io = IoAlert {
            id: "a".into(),
            alert_source_id: "src".into(),
            title: "t".into(),
            description: Some("d".into()),
            status: AlertStatus::Firing,
            deduplication_key: None,
            source_url: None,
            attributes: vec![],
            tags: vec![],
            created_at: None,
        };
        let inc = Incident {
            id: "01X".into(),
            reference: "INC-7".into(),
            name: "Checkout down".into(),
            summary: None,
            incident_status: IncidentStatus {
                id: "s".into(),
                name: "Active".into(),
                category: StatusCategory::Live,
            },
            severity: None,
            permalink: None,
            mode: Some("standard".into()),
            visibility: None,
            created_at: None,
            updated_at: None,
        };
        let alert = to_triage_alert(&io, &[inc]);
        assert_eq!(alert.open_incidents.len(), 1);
        assert_eq!(alert.open_incidents[0].id, "INC-7");
        assert!(alert.source.contains("src"));
    }
}
