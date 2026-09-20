//! Alert triage: one speculative fan-out request, then a routing decision made
//! in code.
//!
//! This module is a worked example of the recommended shape:
//! *code when you can, one judgment per question, decide with thresholds that
//! scale with risk.* Everything the model is not needed for (candidate lookup,
//! threshold policy, output formatting) is ordinary Rust.

mod policy;
mod questions;

pub use policy::{Decision, Policy, decide};
pub use questions::{Impact, NO_DUPLICATE, NONE_OF_THESE, Texts, TriageAnswers, TriageQuestions};

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// An alert as received from a monitoring system. This is the `state`.
///
/// Keep only what the questions need: the model should not have to ignore
/// irrelevant fields, and every field costs input tokens.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Alert {
    /// Emitting system (e.g. `prometheus`, `datadog`).
    pub source: String,
    /// Short alert name.
    pub title: String,
    /// Free-text description or annotation.
    pub description: String,
    /// Labels such as `service`, `namespace`, `severity`.
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
    /// Runbook excerpt, if the alert links one or TechDocs provides one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runbook: Option<String>,
    /// Deploys, config changes or feature flags in the recent window.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recent_changes: Vec<String>,
    /// Incidents currently open, as duplicate candidates.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub open_incidents: Vec<OpenIncident>,
    /// The alerting component as the software catalog knows it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub component: Option<ComponentContext>,
    /// Other alerts firing in the recent window: the blast radius as the
    /// alert hub sees it right now.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub related_alerts: Vec<RelatedAlert>,
}

/// Another alert firing in the same window. Kept to what the impact question
/// and a responder need: what, how long ago, on which component.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelatedAlert {
    /// Title.
    pub title: String,
    /// Minutes since it was created.
    pub age_minutes: u64,
    /// Component label, when the alert carries one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub component: Option<String>,
}

/// A currently open incident that the alert may belong to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenIncident {
    /// Incident identifier (e.g. `INC-4821`).
    pub id: String,
    /// One-line summary.
    pub summary: String,
}

/// Catalog facts about the alerting component. Only what a question reads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ComponentContext {
    /// Catalog name.
    pub name: String,
    /// Display title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// `spec.type` (service, website, library, …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub component_type: Option<String>,
    /// `spec.lifecycle` (production, experimental, deprecated).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lifecycle: Option<String>,
    /// System it belongs to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    /// Owning group's display name. The catalog's answer to "who owns it".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    /// Owning group's description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_description: Option<String>,
    /// What it depends on (`kind name`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,
    /// What depends on it (`kind name`): the blast radius.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dependents: Vec<String>,
    /// Tags.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// Link titles (dashboards, runbooks).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<String>,
}

/// One option for the owner question.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnerCandidate {
    /// Wire key, also used in tags (`ai-team-<key>`).
    pub key: String,
    /// Display name.
    pub label: String,
    /// Rubric text sent as the option's criteria.
    pub description: String,
    /// Catalog reference (`group:default/payments`) when the candidate came
    /// from Backstage; `None` for the static team list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entity_ref: Option<String>,
}

/// The owner question's option set. Always ends with a no-match option.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct OwnerCandidates {
    list: Vec<OwnerCandidate>,
}

impl OwnerCandidates {
    /// Build from candidates; the no-match option is appended if absent.
    pub fn new(mut list: Vec<OwnerCandidate>) -> Self {
        list.retain(|c| c.key != NONE_OF_THESE);
        list.push(OwnerCandidate {
            key: NONE_OF_THESE.to_owned(),
            label: "None of these".to_owned(),
            description: "Not clearly attributable to any listed team from the information given"
                .to_owned(),
            entity_ref: None,
        });
        Self { list }
    }

    /// The built-in fallback list, see [`default_teams`].
    pub fn from_teams() -> Self {
        Self::new(default_teams())
    }

    /// All candidates including the no-match option.
    pub fn iter(&self) -> impl Iterator<Item = &OwnerCandidate> {
        self.list.iter()
    }

    /// Number of candidates including the no-match option.
    pub fn len(&self) -> usize {
        self.list.len()
    }

    /// True when only the no-match option remains.
    pub fn is_empty(&self) -> bool {
        self.list.len() <= 1
    }

    /// Look up by key.
    pub fn get(&self, key: &str) -> Option<&OwnerCandidate> {
        self.list.iter().find(|c| c.key == key)
    }
}

/// The fallback owner candidates used when no software catalog is configured
/// or nothing in it matched. Data, not a type: `[[triage.teams]]` in the
/// configuration file replaces this list without a rebuild.
pub fn default_teams() -> Vec<OwnerCandidate> {
    const TEAMS: [(&str, &str, &str); 6] = [
        (
            "platform",
            "Platform",
            "Kubernetes, cluster add-ons, CI/CD, internal developer platform, cloud accounts and quotas",
        ),
        (
            "database",
            "Database",
            "PostgreSQL, MySQL, Redis, message queues, backups, storage volumes",
        ),
        (
            "network",
            "Network",
            "DNS, load balancers, ingress, CDN, VPN, inter-region connectivity",
        ),
        (
            "application",
            "Application",
            "A product service's own code or configuration: errors, latency, business logic",
        ),
        (
            "security",
            "Security",
            "Suspicious access, secrets exposure, vulnerability findings, policy violations",
        ),
        (
            "observability",
            "Observability",
            "Monitoring, logging or tracing pipeline itself is broken or lagging",
        ),
    ];
    TEAMS
        .iter()
        .map(|(key, label, description)| OwnerCandidate {
            key: (*key).to_owned(),
            label: (*label).to_owned(),
            description: (*description).to_owned(),
            entity_ref: None,
        })
        .collect()
}

/// The owner a decision resolved to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Owner {
    /// Candidate key.
    pub key: String,
    /// Display name.
    pub label: String,
    /// Catalog reference, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entity_ref: Option<String>,
}

impl From<&OwnerCandidate> for Owner {
    fn from(c: &OwnerCandidate) -> Self {
        Self {
            key: c.key.clone(),
            label: c.label.clone(),
            entity_ref: c.entity_ref.clone(),
        }
    }
}
