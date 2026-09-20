//! Alert triage: one speculative fan-out request, then a routing decision made
//! in code.
//!
//! This module is a worked example of the recommended shape:
//! *code when you can, one judgment per question, decide with thresholds that
//! scale with risk.* Everything the model is not needed for (dedup candidate
//! lookup, threshold policy, output formatting) is ordinary Rust.

mod policy;
mod questions;

pub use policy::{Decision, Policy, decide};
pub use questions::{Impact, NO_DUPLICATE, Team, TriageAnswers, TriageQuestions};

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
    /// Runbook excerpt, if the alert links one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runbook: Option<String>,
    /// Deploys, config changes or feature flags in the recent window.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recent_changes: Vec<String>,
    /// Incidents currently open, as duplicate candidates.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub open_incidents: Vec<OpenIncident>,
}

/// A currently open incident that the alert may belong to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenIncident {
    /// Incident identifier (e.g. `INC-4821`).
    pub id: String,
    /// One-line summary.
    pub summary: String,
}
