//! The flow: an alert incident.io received → other alerts firing now →
//! catalog enrichment → recent changes from the feed → TypeSafe judgments →
//! tags, an incident attachment, one qualification note and an owner
//! notification.
//!
//! Follows the webhook docs' rule for keeping systems in sync: the webhook is
//! only a trigger; the alert and the candidate incidents are fetched fresh
//! from the API before judging, so late or reordered deliveries cannot apply
//! a stale decision.

use std::collections::BTreeMap;
use std::time::Duration;

use jiff::{SignedDuration, Timestamp};
use serde::Serialize;

use super::client::Client as IncidentIo;
use super::note::{self, Links, NoteInput};
use super::types::{Alert as IoAlert, Incident};
use crate::backstage::enrich::hints_from_labels;
use crate::backstage::{Enricher, EntityRef};
use crate::changes::{Change, ChangeLog};
use crate::config::DEFAULT_COMPONENT_KEYS;
use crate::triage::{
    Alert, Decision, Impact, OpenIncident, OwnerCandidates, Policy, RelatedAlert, Texts,
    TriageAnswers, TriageQuestions, decide,
};

/// Prefix for every tag this integration writes, so they can be filtered in
/// alert routes and told apart from human tags.
pub const TAG_PREFIX: &str = "ai";

/// How many live incidents to offer as dedup candidates. Each is one Choice
/// option (limit 255) and costs input tokens; recent incidents matter most.
pub const DEFAULT_CANDIDATES: usize = 40;

/// How far back to look for other firing alerts. Half an hour covers a
/// deploy rollout or a dependency failure without pulling in yesterday.
pub const DEFAULT_RELATED_WINDOW: Duration = Duration::from_secs(30 * 60);

/// Most related alerts put in the state. Enough to show a pattern; each one
/// costs input tokens.
pub const DEFAULT_RELATED_MAX: usize = 20;

/// Page size asked for when listing firing alerts; the cap is applied after
/// dropping the alert itself.
const RELATED_PAGE: usize = 50;

/// Which side effects to apply after deciding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteBack {
    /// Add tags, attach duplicates, notify owners.
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
    /// Backstage enrichment, when configured.
    pub backstage: Option<Enricher>,
    /// Notify the owning group through Backstage after `Page`, `Ticket` or
    /// `HumanTriage` decisions. Requires `backstage`.
    pub notify_owner: bool,
    /// Routing thresholds.
    pub policy: Policy,
    /// Question text.
    pub texts: Texts,
    /// Owner candidates when no catalog is configured or nothing matched.
    pub fallback_owners: OwnerCandidates,
    /// Candidate cap for dedup.
    pub max_candidates: usize,
    /// Apply or dry-run.
    pub write_back: WriteBack,
    /// Alert attribute names that identify the component.
    pub component_keys: Vec<String>,
    /// Write the qualification note on the alert.
    pub note: bool,
    /// Window for other firing alerts; zero disables the lookup.
    pub related_window: Duration,
    /// Cap on related alerts put in the state.
    pub related_max: usize,
    /// The change feed, when `POST /changes` is enabled.
    pub changes: Option<ChangeLog>,
    /// How far back a change may lie to be offered as a cause.
    pub change_window: Duration,
    /// Cap on changes put in the state.
    pub change_max: usize,
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
    /// Catalog component the alert was resolved to.
    pub component: Option<String>,
    /// Group notified through Backstage.
    pub notified: Option<String>,
    /// Versioned model that answered.
    pub model: String,
    /// Dedup candidates offered.
    pub candidates_offered: usize,
    /// Owner candidates offered.
    pub owner_candidates_offered: usize,
    /// Other firing alerts put in the state.
    pub related_alerts: usize,
    /// Recent changes put in the state.
    pub recent_changes: usize,
    /// Note written or rewritten on the alert.
    pub note_id: Option<String>,
    /// Seconds from the alert's creation in incident.io to the decision.
    /// `None` when the alert carries no parsable `created_at`.
    pub time_to_qualify_seconds: Option<f64>,
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
    /// Link to the incident in the incident.io app.
    pub permalink: Option<String>,
}

impl Triager {
    /// Build with default policy and candidate cap, without Backstage.
    pub fn new(typesafe: crate::Client, incidentio: IncidentIo) -> Self {
        Self {
            typesafe,
            incidentio,
            backstage: None,
            notify_owner: false,
            policy: Policy::default(),
            texts: Texts::default(),
            fallback_owners: OwnerCandidates::from_teams(),
            max_candidates: DEFAULT_CANDIDATES,
            write_back: WriteBack::Apply,
            component_keys: DEFAULT_COMPONENT_KEYS.map(String::from).to_vec(),
            note: true,
            related_window: DEFAULT_RELATED_WINDOW,
            related_max: DEFAULT_RELATED_MAX,
            changes: None,
            change_window: crate::changes::DEFAULT_WINDOW,
            change_max: crate::changes::DEFAULT_MAX,
        }
    }

    /// Triage an alert by id: fetch, enrich, judge, decide, write back.
    pub async fn triage_alert_by_id(&self, alert_id: &str) -> Result<Outcome, FlowError> {
        let alert = self.incidentio.get_alert(alert_id).await?;
        self.triage_alert(alert).await
    }

    /// Triage an already fetched alert. Prefer [`Self::triage_alert_by_id`]
    /// from a webhook so the state is current.
    #[allow(clippy::too_many_lines)] // one linear flow reads better than fragments
    pub async fn triage_alert(&self, io_alert: IoAlert) -> Result<Outcome, FlowError> {
        let candidates = self
            .incidentio
            .list_open_incidents(self.max_candidates)
            .await?;
        let by_reference: BTreeMap<String, &Incident> = candidates
            .iter()
            .map(|i| (i.reference.clone(), i))
            .collect();

        let mut alert = to_triage_alert(&io_alert, &candidates);

        // Blast radius: what else is firing right now. Context only, so a
        // failure here degrades to an empty list rather than aborting.
        let now = Timestamp::now();
        alert.related_alerts = self.related_alerts(&io_alert, now).await;

        // Catalog enrichment: component context, owner candidates, runbook.
        let mut owner_candidates = self.fallback_owners.clone();
        let mut component_name = None;
        let mut links = Links::default();
        if let Some(enricher) = &self.backstage {
            let hints = hints_from_labels(&alert.labels, &self.component_keys);
            let text = format!("{} {}", alert.title, alert.description);
            let enrichment = enricher.enrich(&hints, &text).await?;
            component_name = enrichment.component.as_ref().map(|c| c.name.clone());
            if enrichment.runbook.is_some() {
                alert.runbook = enrichment.runbook;
            }
            links.component = enrichment.component_url;
            links.runbook = enrichment.runbook_url;
            alert.component = enrichment.component;
            owner_candidates = enrichment.candidates;
            tracing::debug!(
                alert_id = %io_alert.id,
                component = ?component_name,
                matched_by = ?enrichment.matched_by,
                owner_candidates = owner_candidates.len(),
                "catalog enrichment"
            );
        }

        // Recent changes: what the feed knows about this component and the
        // platform. Only when the alert itself carried none, so a source that
        // lists its own changes keeps them.
        if alert.recent_changes.is_empty()
            && let Some(log) = &self.changes
        {
            let mut hints = hints_from_labels(&alert.labels, &self.component_keys);
            if let Some(c) = &alert.component
                && !hints.iter().any(|h| h.eq_ignore_ascii_case(&c.name))
            {
                hints.push(c.name.clone());
            }
            alert.recent_changes = log
                .recent(&hints, self.change_window, now, self.change_max)
                .iter()
                .map(Change::to_state_line)
                .collect();
        }

        let questions =
            TriageQuestions::for_alert_with_texts(&alert, owner_candidates, &self.texts)?;
        let state = TriageQuestions::state(&alert);
        let response = self
            .typesafe
            .system_one(&state, &questions.questions)
            .await?;
        let answers = questions.read(&response)?;
        let decision = decide(&answers, &self.policy);
        let time_to_qualify = time_since(io_alert.created_at.as_deref(), Timestamp::now());
        if let (Some(enricher), Some(owner)) = (&self.backstage, decision.owner()) {
            links.owner = owner
                .entity_ref
                .as_deref()
                .and_then(|r| EntityRef::parse(r, "group"))
                .map(|r| enricher.entity_url(&r));
        }

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
                    permalink: inc.permalink.clone(),
                });
            } else {
                tracing::warn!(
                    reference,
                    "dedup chose a reference not in the candidate map"
                );
            }
        }

        let applied = self.write_back == WriteBack::Apply;
        let mut notified = None;
        let mut note_id = None;
        if applied {
            self.incidentio.add_alert_tags(&io_alert.id, &tags).await?;
            if let Some(a) = &attached_to {
                self.incidentio
                    .attach_alert_to_incident(&io_alert.id, &a.id)
                    .await?;
            }
            if self.note {
                let content = note::render(&NoteInput {
                    alert: &io_alert,
                    answers: &answers,
                    decision: &decision,
                    attached: attached_to.as_ref(),
                    component: alert.component.as_ref(),
                    links: &links,
                    related: &alert.related_alerts,
                    related_window: self.related_window,
                    changes: &alert.recent_changes,
                    tags: &tags,
                    time_to_qualify,
                });
                // The note is a convenience on top of the tags; its failure
                // must not undo what was already written.
                match self.write_note(&io_alert.id, &content).await {
                    Ok(id) => note_id = Some(id),
                    Err(e) => tracing::warn!(error = %e, "qualification note failed"),
                }
            }
            if self.notify_owner
                && let Some(enricher) = &self.backstage
                && let Some(owner) = decision.owner()
                && let Some(candidate) = answers.candidates.get(&owner.key)
            {
                match enricher
                    .notify_owner(
                        candidate,
                        &io_alert.title,
                        &decision,
                        io_alert.source_url.clone(),
                    )
                    .await
                {
                    Ok(true) => notified = candidate.entity_ref.clone(),
                    Ok(false) => {}
                    // A failed notification must not undo the tags already written.
                    Err(e) => tracing::warn!(error = %e, "owner notification failed"),
                }
            }
        }

        tracing::info!(
            alert_id = %io_alert.id,
            title = %io_alert.title,
            decision = ?decision,
            component = ?component_name,
            attached = ?attached_to.as_ref().map(|a| &a.reference),
            notified = ?notified,
            note = ?note_id,
            related_alerts = alert.related_alerts.len(),
            recent_changes = alert.recent_changes.len(),
            time_to_qualify_seconds = ?time_to_qualify.map(|d| d.as_secs_f64()),
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
            component: component_name,
            notified,
            model: response.model,
            candidates_offered: candidates.len(),
            owner_candidates_offered: answers.candidates.len(),
            related_alerts: alert.related_alerts.len(),
            recent_changes: alert.recent_changes.len(),
            note_id,
            time_to_qualify_seconds: time_to_qualify.map(|d| d.as_secs_f64()),
            applied,
        })
    }

    /// Other alerts firing in the window, newest first, without this one.
    async fn related_alerts(&self, io_alert: &IoAlert, now: Timestamp) -> Vec<RelatedAlert> {
        if self.related_window.is_zero() || self.related_max == 0 {
            return Vec::new();
        }
        let Ok(window) = SignedDuration::try_from(self.related_window) else {
            return Vec::new();
        };
        let Ok(since) = now.checked_sub(window) else {
            return Vec::new();
        };
        let since = since.strftime("%Y-%m-%dT%H:%M:%SZ").to_string();
        match self
            .incidentio
            .list_firing_alerts_since(&since, RELATED_PAGE)
            .await
        {
            Ok(alerts) => related_from(
                &alerts,
                &io_alert.id,
                &self.component_keys,
                now,
                self.related_max,
            ),
            Err(e) => {
                tracing::warn!(error = %e, "listing related alerts failed; continuing without");
                Vec::new()
            }
        }
    }

    /// Create the note, or rewrite the one signalman left on an earlier pass.
    async fn write_note(&self, alert_id: &str, content: &str) -> super::error::Result<String> {
        let existing = self.incidentio.list_alert_notes(alert_id).await?;
        if let Some(mine) = existing
            .iter()
            .find(|n| note::is_signalman_note(&n.content))
        {
            let updated = self.incidentio.update_alert_note(&mine.id, content).await?;
            return Ok(updated.id);
        }
        let created = self.incidentio.create_alert_note(alert_id, content).await?;
        Ok(created.id)
    }
}

/// Shape firing alerts as state, newest first, excluding `exclude_id`.
pub fn related_from(
    alerts: &[IoAlert],
    exclude_id: &str,
    component_keys: &[String],
    now: Timestamp,
    max: usize,
) -> Vec<RelatedAlert> {
    let mut related: Vec<(u64, RelatedAlert)> = alerts
        .iter()
        .filter(|a| a.id != exclude_id)
        .map(|a| {
            let age_minutes =
                time_since(a.created_at.as_deref(), now).map_or(u64::MAX, |d| d.as_secs() / 60);
            let labels = a.labels();
            let component = hints_from_labels(&labels, component_keys)
                .into_iter()
                .next();
            (
                age_minutes,
                RelatedAlert {
                    title: a.title.clone(),
                    age_minutes: if age_minutes == u64::MAX {
                        0
                    } else {
                        age_minutes
                    },
                    component,
                },
            )
        })
        .collect();
    related.sort_by_key(|(age, _)| *age);
    related.into_iter().map(|(_, r)| r).take(max).collect()
}

/// Time from an RFC 3339 instant to `now`; `None` when absent, unparsable or
/// in the future (clock skew is not a negative duration).
pub fn time_since(created_at: Option<&str>, now: Timestamp) -> Option<Duration> {
    let created: Timestamp = created_at?.parse().ok()?;
    let elapsed = now.duration_since(created);
    Duration::try_from(elapsed).ok()
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
        component: None,
        related_alerts: vec![],
    }
}

/// Tags encoding the judgments and the decision.
pub fn tags_for(answers: &TriageAnswers, decision: &Decision) -> Vec<String> {
    let impact = Impact::from_level(answers.impact.nearest_level());
    let mut tags = vec![
        tag("team", &answers.owner.chosen),
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

fn impact_key(i: Impact) -> &'static str {
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
    /// Backstage API.
    #[error(transparent)]
    Backstage(#[from] crate::backstage::Error),
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
        assert!(alert.component.is_none());
        assert!(alert.related_alerts.is_empty());
    }

    fn firing(id: &str, title: &str, created_at: &str) -> IoAlert {
        IoAlert {
            id: id.into(),
            alert_source_id: "src".into(),
            title: title.into(),
            description: None,
            status: AlertStatus::Firing,
            deduplication_key: None,
            source_url: None,
            attributes: vec![],
            tags: vec![],
            created_at: Some(created_at.into()),
        }
    }

    #[test]
    fn related_alerts_exclude_self_sort_by_age_and_cap() {
        let now: Timestamp = "2026-09-20T12:00:00Z".parse().unwrap();
        let alerts = [
            firing("me", "this one", "2026-09-20T11:59:00Z"),
            firing("b", "older", "2026-09-20T11:40:00Z"),
            firing("c", "newer", "2026-09-20T11:55:30Z"),
            firing("d", "unparsable", "not a time"),
        ];
        let keys = DEFAULT_COMPONENT_KEYS.map(String::from);
        let related = related_from(&alerts, "me", &keys, now, 2);
        assert_eq!(related.len(), 2);
        assert_eq!(related[0].title, "newer");
        assert_eq!(related[0].age_minutes, 4);
        assert_eq!(related[1].title, "older");
        assert_eq!(related[1].age_minutes, 20);
    }

    #[test]
    fn time_since_handles_missing_bad_and_future_instants() {
        let now: Timestamp = "2026-09-20T12:00:00Z".parse().unwrap();
        assert_eq!(
            time_since(Some("2026-09-20T11:59:18Z"), now),
            Some(Duration::from_secs(42))
        );
        assert_eq!(time_since(None, now), None);
        assert_eq!(time_since(Some("yesterday"), now), None);
        assert_eq!(time_since(Some("2026-09-20T12:00:01Z"), now), None);
    }
}
