//! Wire types for the subset of the incident.io API this integration uses.
//!
//! Unknown fields are ignored and most optional fields default, because the
//! API adds properties over time and a webhook payload must never fail to
//! parse over a field we do not read.

use std::collections::BTreeMap;
use std::fmt::Write;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Longest incident summary offered to the model as a dedup candidate.
pub const CANDIDATE_SUMMARY_CHARS: usize = 280;

/// Lifecycle category of an incident status (API names; "live" shows as
/// "Active" in the app, "learning" as "Post-incident").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StatusCategory {
    /// Being assessed.
    Triage,
    /// Declined at triage.
    Declined,
    /// Merged into another incident.
    Merged,
    /// Canceled.
    Canceled,
    /// Active.
    Live,
    /// Post-incident.
    Learning,
    /// Closed.
    Closed,
    /// Paused.
    Paused,
    /// A category this build does not know.
    #[serde(other)]
    Unknown,
}

impl StatusCategory {
    /// The query-string value for `status_category[one_of]`.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Triage => "triage",
            Self::Declined => "declined",
            Self::Merged => "merged",
            Self::Canceled => "canceled",
            Self::Live => "live",
            Self::Learning => "learning",
            Self::Closed => "closed",
            Self::Paused => "paused",
            Self::Unknown => "unknown",
        }
    }
}

/// Embedded incident status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IncidentStatus {
    /// Status id.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Lifecycle category.
    pub category: StatusCategory,
}

/// Embedded severity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Severity {
    /// Severity id.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Ordering, higher is more severe.
    #[serde(default)]
    pub rank: i64,
}

/// An incident, as returned by `GET /v2/incidents` and in webhooks.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Incident {
    /// ULID.
    pub id: String,
    /// Human reference such as `INC-123`.
    pub reference: String,
    /// Title.
    pub name: String,
    /// Longer description.
    #[serde(default)]
    pub summary: Option<String>,
    /// Current status.
    pub incident_status: IncidentStatus,
    /// Current severity, if set.
    #[serde(default)]
    pub severity: Option<Severity>,
    /// Link to the incident homepage.
    #[serde(default)]
    pub permalink: Option<String>,
    /// `standard`, `retrospective`, `test` or `tutorial`.
    #[serde(default)]
    pub mode: Option<String>,
    /// `public` or `private`.
    #[serde(default)]
    pub visibility: Option<String>,
    /// Creation time (RFC 3339).
    #[serde(default)]
    pub created_at: Option<String>,
    /// Last update time (RFC 3339).
    #[serde(default)]
    pub updated_at: Option<String>,
}

impl Incident {
    /// True for real incidents in an open category.
    pub fn is_open_and_real(&self) -> bool {
        matches!(
            self.incident_status.category,
            StatusCategory::Triage | StatusCategory::Live | StatusCategory::Paused
        ) && self.mode.as_deref().is_none_or(|m| m == "standard")
    }

    /// One-line description for a dedup candidate.
    pub fn candidate_summary(&self) -> String {
        let mut s = self.name.clone();
        if let Some(sev) = &self.severity {
            let _ = write!(s, " [{}]", sev.name);
        }
        if let Some(sum) = self
            .summary
            .as_deref()
            .map(str::trim)
            .filter(|x| !x.is_empty())
        {
            s.push_str(": ");
            // Live summaries are whole markdown bodies; forty of them would
            // dominate the request. The first line or so is what identifies
            // an incident. ponytail: fixed cut, make it a setting if it bites.
            let cut = sum
                .char_indices()
                .nth(CANDIDATE_SUMMARY_CHARS)
                .map_or(sum.len(), |(i, _)| i);
            s.push_str(
                sum[..cut]
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
                    .as_str(),
            );
            if cut < sum.len() {
                s.push('…');
            }
        }
        s
    }
}

/// Alert status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlertStatus {
    /// Still firing.
    Firing,
    /// Resolved upstream.
    Resolved,
}

/// One value of an alert attribute.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct AttributeValue {
    /// Literal value, when the attribute is not a catalog reference.
    #[serde(default)]
    pub literal: Option<String>,
    /// Display label.
    #[serde(default)]
    pub label: Option<String>,
    /// Catalog entry, when the attribute references one.
    #[serde(default)]
    pub catalog_entry: Option<CatalogEntryRef>,
}

impl AttributeValue {
    /// Best human-readable rendering.
    pub fn display(&self) -> Option<&str> {
        self.label
            .as_deref()
            .or(self.literal.as_deref())
            .or(self.catalog_entry.as_ref().map(|c| c.name.as_str()))
    }
}

/// Reference to a catalog entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogEntryRef {
    /// Entry id.
    pub id: String,
    /// Entry name.
    pub name: String,
    /// Catalog type id.
    #[serde(default)]
    pub catalog_type_id: Option<String>,
}

/// Definition of an alert attribute.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttributeDef {
    /// Attribute id.
    pub id: String,
    /// Attribute name, e.g. `Service`.
    pub name: String,
    /// Whether the value is an array.
    #[serde(default)]
    pub array: bool,
}

/// An attribute on an alert.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttributeEntry {
    /// Definition.
    pub attribute: AttributeDef,
    /// Scalar value.
    #[serde(default)]
    pub value: Option<AttributeValue>,
    /// Array value.
    #[serde(default)]
    pub array_value: Option<Vec<AttributeValue>>,
}

impl AttributeEntry {
    /// Render the value(s) as one string, arrays comma-joined.
    pub fn display(&self) -> Option<String> {
        if let Some(v) = &self.value {
            return v.display().map(str::to_owned);
        }
        let joined: Vec<&str> = self
            .array_value
            .as_deref()
            .unwrap_or_default()
            .iter()
            .filter_map(AttributeValue::display)
            .collect();
        (!joined.is_empty()).then(|| joined.join(", "))
    }
}

/// A tag on an alert.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tag {
    /// Tag id.
    pub id: String,
    /// Tag name.
    pub name: String,
}

/// An alert, as returned by `GET /v2/alerts/{id}` and in webhooks.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Alert {
    /// ULID.
    pub id: String,
    /// The alert source that produced it.
    pub alert_source_id: String,
    /// Title.
    pub title: String,
    /// Description (markdown).
    #[serde(default)]
    pub description: Option<String>,
    /// Firing or resolved.
    pub status: AlertStatus,
    /// Upstream dedup key.
    #[serde(default)]
    pub deduplication_key: Option<String>,
    /// Link to the upstream alert.
    #[serde(default)]
    pub source_url: Option<String>,
    /// Parsed attributes.
    #[serde(default)]
    pub attributes: Vec<AttributeEntry>,
    /// Applied tags.
    #[serde(default)]
    pub tags: Vec<Tag>,
    /// Creation time.
    #[serde(default)]
    pub created_at: Option<String>,
}

impl Alert {
    /// Attributes as a flat `name -> value` map for the triage state.
    pub fn labels(&self) -> BTreeMap<String, String> {
        self.attributes
            .iter()
            .filter_map(|a| a.display().map(|v| (a.attribute.name.clone(), v)))
            .collect()
    }

    /// Tag names.
    pub fn tag_names(&self) -> Vec<String> {
        self.tags.iter().map(|t| t.name.clone()).collect()
    }
}

/// Link between an alert and an incident.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IncidentAlert {
    /// Connection id.
    pub id: String,
    /// Route that created it, if any.
    #[serde(default)]
    pub alert_route_id: Option<String>,
}

/// Body of `POST /v2/alert_events/http/{alert_source_config_id}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AlertEvent {
    /// Title.
    pub title: String,
    /// Description (markdown).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Firing or resolved.
    pub status: AlertStatus,
    /// Stable key so repeated events update one alert.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deduplication_key: Option<String>,
    /// Link to the upstream alert.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_url: Option<String>,
    /// Free-form fields the alert source's attribute templates read.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,
}

/// Response of the alert events endpoint (HTTP 202).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlertEventAck {
    /// Dedup key the event was processed with.
    pub deduplication_key: String,
    /// Human-readable detail.
    pub message: String,
    /// Event status.
    pub status: String,
}

/// `GET /v1/identity`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Identity {
    /// API key name.
    pub name: String,
    /// Roles granted to the key.
    #[serde(default)]
    pub roles: Vec<String>,
    /// Dashboard URL.
    #[serde(default)]
    pub dashboard_url: Option<String>,
}

// Envelope shapes used by the client.

#[derive(Deserialize)]
pub(crate) struct IncidentsPage {
    pub incidents: Vec<Incident>,
    #[serde(default)]
    pub pagination_meta: Option<PaginationMeta>,
}

#[derive(Deserialize)]
pub(crate) struct AlertsPage {
    pub alerts: Vec<Alert>,
}

#[derive(Deserialize, Default)]
pub(crate) struct PaginationMeta {
    #[serde(default)]
    pub after: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct AlertEnvelope {
    pub alert: Alert,
}

#[derive(Deserialize)]
pub(crate) struct IncidentAlertEnvelope {
    pub incident_alert: IncidentAlert,
}

#[derive(Deserialize)]
pub(crate) struct IdentityEnvelope {
    pub identity: Identity,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use serde_json::json;

    #[test]
    fn incident_parses_documented_example_and_ignores_extras() {
        let v = json!({
            "id": "01FDAG4SAP5TYPT98WGR2N7W91", "reference": "INC-123", "name": "Our database is sad",
            "summary": "really sad", "permalink": "https://app.incident.io/incidents/123",
            "incident_status": {"category": "live", "id": "s1", "name": "Investigating", "rank": 1,
                                "created_at": "x", "updated_at": "y", "description": "d"},
            "severity": {"id": "sev1", "name": "Minor", "rank": 1, "created_at": "x", "updated_at": "y", "description": "d"},
            "mode": "standard", "visibility": "public", "workload_minutes_total": 60.7,
            "custom_field_entries": [], "team_ids": ["t1"]
        });
        let mut inc: Incident = serde_json::from_value(v).unwrap();
        assert!(inc.is_open_and_real());
        assert_eq!(
            inc.candidate_summary(),
            "Our database is sad [Minor]: really sad"
        );
        // A live summary is a markdown body: one line, cut, marked as cut.
        inc.summary = Some(format!(
            "**Alert:** first\n\n**Context:** {}",
            "x ".repeat(300)
        ));
        let s = inc.candidate_summary();
        assert!(s.starts_with("Our database is sad [Minor]: **Alert:** first **Context:** x x"));
        assert!(s.ends_with('…'), "{s}");
        assert!(
            s.chars().count() < CANDIDATE_SUMMARY_CHARS + 40,
            "{}",
            s.len()
        );
        let unknown: StatusCategory = serde_json::from_value(json!("brand_new")).unwrap();
        assert_eq!(unknown, StatusCategory::Unknown);
    }

    #[test]
    fn test_and_closed_incidents_are_not_candidates() {
        let mut inc: Incident = serde_json::from_value(json!({
            "id": "i", "reference": "INC-1", "name": "n",
            "incident_status": {"id": "s", "name": "Closed", "category": "closed"}
        }))
        .unwrap();
        assert!(!inc.is_open_and_real());
        inc.incident_status.category = StatusCategory::Live;
        inc.mode = Some("test".into());
        assert!(!inc.is_open_and_real());
    }

    #[test]
    fn alert_attributes_flatten_to_labels() {
        let alert: Alert = serde_json::from_value(json!({
            "id": "a1", "alert_source_id": "src", "title": "CPU high", "status": "firing",
            "deduplication_key": "k", "attributes": [
                {"attribute": {"id": "x", "name": "Service", "array": false, "required": false, "type": "CatalogEntry[\"Service\"]"},
                 "value": {"catalog_entry": {"id": "c", "name": "checkout-api", "catalog_type_id": "t"}, "label": "checkout-api"}},
                {"attribute": {"id": "y", "name": "Regions", "array": true, "required": false, "type": "String"},
                 "array_value": [{"literal": "eu-1"}, {"literal": "eu-2"}]}
            ],
            "tags": [{"id": "t1", "name": "noisy"}]
        }))
        .unwrap();
        let labels = alert.labels();
        assert_eq!(labels["Service"], "checkout-api");
        assert_eq!(labels["Regions"], "eu-1, eu-2");
        assert_eq!(alert.tag_names(), vec!["noisy"]);
    }
}

/// A note on an alert (`/v1/alert_notes`): a markdown body a responder reads
/// in the alert view. signalman keeps exactly one per alert and rewrites it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlertNote {
    /// ULID.
    pub id: String,
    /// The alert it is attached to; `None` for alert-group notes.
    #[serde(default)]
    pub alert_id: Option<String>,
    /// Markdown body.
    pub content: String,
    /// Creation time (RFC 3339).
    #[serde(default)]
    pub created_at: Option<String>,
    /// Last update time (RFC 3339).
    #[serde(default)]
    pub updated_at: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct AlertNoteEnvelope {
    pub alert_note: AlertNote,
}

#[derive(Deserialize)]
pub(crate) struct AlertNotesPage {
    pub alert_notes: Vec<AlertNote>,
}
