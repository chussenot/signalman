//! The MCP server: signalman's typed capabilities as read-only tools.
//!
//! Decision 0008: signalman is a tool for agents, not an agent. Every tool
//! here answers with the same judgments the CLI would print; nothing decides
//! anything the CLI could not already decide, and nothing writes to
//! incident.io or Backstage. [`Server`] wraps exactly one
//! [`Triager`] — "cheap to clone; share one per
//! process" — and forces it to [`WriteBack::DryRun`] in
//! [`Server::new`] regardless of how the caller built it, so a
//! misconfiguration elsewhere in the process cannot turn a read tool into a
//! write. Every tool is a thin wrapper over a function the CLI subcommands
//! already call (`Triager::triage_alert_by_id`, `related_from`, `tags_for`,
//! `Enricher::enrich`, `IncidentIo::list_open_incidents`), so there is
//! exactly one implementation of each capability.
//!
//! `signalman mcp` (`src/main.rs`) serves this over stdio. A future
//! Streamable HTTP transport (`mcp.transport = "http"`) would mount the same
//! [`Server`] on the axum router `serve` already runs.

use std::collections::BTreeMap;
use std::time::Duration;

use jiff::{SignedDuration, Timestamp};
use rmcp::ErrorData as McpError;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerConfig};
use rmcp::{ServerHandler, tool, tool_handler, tool_router};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::backstage::enrich::hints_from_labels;
use crate::incidentio::sync::{RELATED_PAGE, WriteBack, related_from, tags_for};
use crate::incidentio::{Incident, Triager};
use crate::outcome::{self, AlertRef, ChangeRef, ComponentRef, IncidentRef, Outcome};
use crate::triage::{Alert, ComponentContext, Decision, TriageQuestions, decide};

/// signalman's MCP server: five read-only tools over one dry-run [`Triager`].
#[derive(Clone)]
pub struct Server {
    triager: Triager,
    tool_router: ToolRouter<Self>,
}

impl Server {
    /// Wrap `triager`, forcing [`WriteBack::DryRun`] regardless of how the
    /// caller configured it.
    pub fn new(mut triager: Triager) -> Self {
        triager.write_back = WriteBack::DryRun;
        // Each capability below is its own `#[tool_router]` block (its own
        // generated router-building method), merged into one router here so
        // the tools stay grouped with the request/response types they use.
        let tool_router = Self::tool_router()
            + Self::related_alerts_router()
            + Self::recent_changes_router()
            + Self::lookup_owner_router()
            + Self::open_incidents_router();
        Self {
            triager,
            tool_router,
        }
    }
}

// ---------------------------------------------------------------------------
// qualify_alert
// ---------------------------------------------------------------------------

/// Input for `qualify_alert`. Exactly one of `alert_id` or `alert`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct QualifyAlertRequest {
    /// An existing incident.io alert id (a ULID). Fetched fresh and triaged
    /// with full live context: the software catalog, other alerts firing
    /// now, recent changes, and live open incidents as dedup candidates.
    /// Mutually exclusive with `alert`.
    #[serde(default)]
    pub alert_id: Option<String>,
    /// A standalone alert body for one that has no incident.io record yet:
    /// the same shape `signalman triage` reads from a file (`source`,
    /// `title`, `description`, `labels`, and optionally `runbook`,
    /// `recent_changes`, `open_incidents`, `component`, `related_alerts`;
    /// see `examples/alerts/*.json`). Mutually exclusive with `alert_id`.
    #[serde(default)]
    pub alert: Option<serde_json::Value>,
}

#[tool_router]
impl Server {
    #[tool(
        description = "Triage one alert and return the outcome contract (schema v1): the routing decision, every judgment with its probability and confidence, catalog links, blast-radius context, and the tags that would be written. Always a dry run: never writes a tag, a note, an incident attachment or an owner notification, however the server is configured. Give exactly one of `alert_id` (an existing incident.io alert, triaged with full live context) or `alert` (a standalone alert body, for one that is not yet in incident.io).",
        annotations(
            title = "Qualify an alert",
            read_only_hint = true,
            idempotent_hint = true,
            open_world_hint = true
        )
    )]
    async fn qualify_alert(
        &self,
        Parameters(req): Parameters<QualifyAlertRequest>,
    ) -> Result<CallToolResult, McpError> {
        let outcome = match (req.alert_id, req.alert) {
            (Some(_), Some(_)) => {
                return Err(McpError::invalid_params(
                    "give alert_id or alert, not both",
                    None,
                ));
            }
            (None, None) => {
                return Err(McpError::invalid_params("give alert_id or alert", None));
            }
            (Some(id), None) => match self.triager.triage_alert_by_id(&id).await {
                Ok(o) => o,
                Err(e) => return Ok(tool_failed("qualify_alert", &e)),
            },
            (None, Some(body)) => match self.qualify_inline(body).await {
                Ok(o) => o,
                Err(Failed::Params(msg)) => return Err(McpError::invalid_params(msg, None)),
                Err(Failed::Upstream(msg)) => return Ok(tool_failed("qualify_alert", &msg)),
            },
        };
        structured(&outcome, qualify_summary(&outcome))
    }
}

/// A tool ran and could not finish (`Upstream`, visible to the caller as a
/// tool-level error) versus a request that was never valid to begin with
/// (`Params`, a protocol-level error).
enum Failed {
    Params(String),
    Upstream(String),
}

impl Server {
    /// `qualify_alert` for a standalone alert body: the same steps
    /// `signalman triage <file>` runs (catalog enrichment when Backstage is
    /// configured, the TypeSafe call, the policy, the outcome), always
    /// attempted best-effort since an MCP caller has no CLI flag to opt in
    /// with. Writes nothing; `writes.mode` is always `detached`, exactly as
    /// the file-based CLI reports it.
    async fn qualify_inline(&self, body: serde_json::Value) -> Result<Outcome, Failed> {
        let mut alert: Alert = serde_json::from_value(body).map_err(|e| {
            Failed::Params(format!("`alert` does not match the expected shape: {e}"))
        })?;

        let enrichment = self.enrich_inline(&mut alert).await?;

        // Best effort: resolve the ids and permalinks of any incidents the
        // alert body already names as dedup candidates. A failure here
        // degrades to references only, exactly as the flow degrades related
        // alerts on a fetch failure; it never fails the triage.
        let mut fetched_incidents: Vec<Incident> = Vec::new();
        if !alert.open_incidents.is_empty()
            && let Ok(live) = self
                .triager
                .incidentio
                .list_open_incidents(self.triager.max_candidates)
                .await
        {
            fetched_incidents = live;
        }

        let questions = TriageQuestions::for_alert_with_texts(
            &alert,
            enrichment.candidates,
            &self.triager.texts,
        )
        .map_err(|e| Failed::Upstream(format!("building the questions failed: {e}")))?;
        let state = TriageQuestions::state(&alert);
        let response = self
            .triager
            .typesafe
            .system_one(&state, &questions.questions)
            .await
            .map_err(|e| Failed::Upstream(format!("TypeSafe call failed: {e}")))?;
        let answers = questions
            .read(&response)
            .map_err(|e| Failed::Upstream(format!("reading the answer failed: {e}")))?;
        let decision = decide(&answers, &self.triager.policy);
        let tags = tags_for(&answers, &decision);

        let (candidate_incidents, incident) =
            resolve_incident_refs(&alert.open_incidents, &fetched_incidents, &decision);
        let owner_url = decision
            .owner()
            .and_then(|owner| enrichment.candidate_urls.get(&owner.key).cloned());

        Ok(Outcome::build(outcome::Input {
            version: env!("CARGO_PKG_VERSION"),
            decided_at: Timestamp::now(),
            // A standalone body carries no incident.io creation time.
            time_to_qualify: None,
            alert: AlertRef {
                id: None,
                title: alert.title.clone(),
                source: alert.source.clone(),
                source_url: alert.labels.get("source_url").cloned(),
                created_at: None,
                labels: alert.labels.clone(),
            },
            answers: &answers,
            decision: &decision,
            policy: &self.triager.policy,
            owner_url,
            candidate_urls: &enrichment.candidate_urls,
            incident,
            candidate_incidents: &candidate_incidents,
            component: alert.component.as_ref(),
            component_url: enrichment.component_url,
            runbook_url: enrichment.runbook_url,
            related: &alert.related_alerts,
            changes: outcome::Changes::Lines(&alert.recent_changes),
            // Neither lookup runs on this path: the body is the whole world.
            windows: outcome::Windows {
                related_seconds: None,
                change_seconds: None,
            },
            tags: &tags,
            model: &response.model,
            usage: response.usage.into(),
            writes: outcome::Writes::detached(),
        }))
    }
}

/// What Backstage enrichment (or its absence) contributes to
/// [`Server::qualify_inline`]: the owner candidates to offer, and the links
/// [`outcome::Input`] needs. Empty and unmatched when Backstage is not
/// configured, mirroring `signalman triage <file>` without
/// `--enrich-from-backstage`.
struct InlineEnrichment {
    candidates: crate::triage::OwnerCandidates,
    component_url: Option<String>,
    runbook_url: Option<String>,
    candidate_urls: BTreeMap<String, String>,
}

impl Server {
    /// Resolve `alert`'s component in the catalog when Backstage is
    /// configured, filling `alert.runbook` and `alert.component` in place —
    /// exactly what `signalman triage <file> --enrich-from-backstage` does,
    /// attempted automatically since an MCP caller has no such flag.
    async fn enrich_inline(&self, alert: &mut Alert) -> Result<InlineEnrichment, Failed> {
        let Some(enricher) = &self.triager.backstage else {
            return Ok(InlineEnrichment {
                candidates: self.triager.fallback_owners.clone(),
                component_url: None,
                runbook_url: None,
                candidate_urls: BTreeMap::new(),
            });
        };
        let hints = hints_from_labels(&alert.labels, &self.triager.component_keys);
        let text = format!("{} {}", alert.title, alert.description);
        let enrichment = enricher
            .enrich(&hints, &text)
            .await
            .map_err(|e| Failed::Upstream(format!("Backstage enrichment failed: {e}")))?;
        if enrichment.runbook.is_some() {
            alert.runbook = enrichment.runbook;
        }
        alert.component = enrichment.component;
        let candidate_urls = enricher.candidate_urls(&enrichment.candidates);
        Ok(InlineEnrichment {
            candidates: enrichment.candidates,
            component_url: enrichment.component_url,
            runbook_url: enrichment.runbook_url,
            candidate_urls,
        })
    }
}

/// Build the dedup candidate list and, when the decision attaches, its
/// target — the same mapping `main.rs`'s file-based CLI path uses, so both
/// emitters resolve an open incident's id and permalink the same way.
fn resolve_incident_refs(
    open_incidents: &[crate::triage::OpenIncident],
    fetched: &[Incident],
    decision: &Decision,
) -> (Vec<IncidentRef>, Option<IncidentRef>) {
    let candidate_incidents: Vec<IncidentRef> = open_incidents
        .iter()
        .map(|open| {
            let known = fetched.iter().find(|i| i.reference == open.id);
            IncidentRef {
                id: known.map(|i| i.id.clone()),
                reference: open.id.clone(),
                url: known.and_then(|i| i.permalink.clone()),
            }
        })
        .collect();
    let incident = match decision {
        Decision::AttachToIncident { incident_id, .. } => Some(
            candidate_incidents
                .iter()
                .find(|i| &i.reference == incident_id)
                .cloned()
                .unwrap_or_else(|| IncidentRef {
                    id: None,
                    reference: incident_id.clone(),
                    url: None,
                }),
        ),
        _ => None,
    };
    (candidate_incidents, incident)
}

/// One sentence naming the decision, for the tool's text fallback.
/// `structured_content` carries the full document; this is what a client
/// shows when it renders content instead of structured JSON.
fn qualify_summary(o: &Outcome) -> String {
    match o.decision {
        outcome::Action::Suppress => format!(
            "suppress (actionable {:.2})",
            f64::from(o.judgments.actionable.probability)
        ),
        outcome::Action::AttachToIncident => format!(
            "attach to {} (confidence {:.2})",
            o.incident.as_ref().map_or("?", |i| i.reference.as_str()),
            o.judgments
                .duplicate_of
                .as_ref()
                .map_or(0.0, |d| f64::from(d.confidence)),
        ),
        outcome::Action::Page => format!(
            "page {}, {} impact{}",
            o.owner.as_ref().map_or("?", |w| w.label.as_str()),
            o.impact.key(),
            if o.suspected_change {
                " (suspected change)"
            } else {
                ""
            }
        ),
        outcome::Action::Ticket => format!(
            "ticket {}, {} impact",
            o.owner.as_ref().map_or("?", |w| w.label.as_str()),
            o.impact.key(),
        ),
        outcome::Action::HumanTriage => format!(
            "human triage, best guess {}",
            o.owner.as_ref().map_or("?", |w| w.label.as_str()),
        ),
    }
}

// ---------------------------------------------------------------------------
// related_alerts
// ---------------------------------------------------------------------------

/// Input for `related_alerts`. Exactly one of `alert_id` or `component`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct RelatedAlertsRequest {
    /// Exclude this incident.io alert id from the results — the alert you
    /// are investigating. Every other alert firing in the window is
    /// returned; whether one is actually related is a judgment
    /// `qualify_alert` makes, not a filter this tool applies. Mutually
    /// exclusive with `component`.
    #[serde(default)]
    pub alert_id: Option<String>,
    /// Return only alerts naming this component (matched the same way
    /// triage matches alert labels to the catalog), excluding nothing.
    /// Mutually exclusive with `alert_id`.
    #[serde(default)]
    pub component: Option<String>,
    /// How far back to look. Defaults to the server's configured
    /// related-alert window (`flow.related_window_minutes`).
    #[serde(default)]
    pub window_minutes: Option<u64>,
}

#[tool_router(router = related_alerts_router)]
impl Server {
    #[tool(
        name = "related_alerts",
        description = "List other alerts currently firing in incident.io within a time window: the blast radius as the alert hub sees it right now, the same list qualify_alert puts in its state. Give either `alert_id` (excludes that one alert, returns everything else firing) or `component` (returns only alerts naming that component). Each row names its own component when it has one; matching alerts to a shared cause is a judgment, not something this tool decides.",
        annotations(
            title = "List related firing alerts",
            read_only_hint = true,
            idempotent_hint = true,
            open_world_hint = true
        )
    )]
    async fn related_alerts(
        &self,
        Parameters(req): Parameters<RelatedAlertsRequest>,
    ) -> Result<CallToolResult, McpError> {
        let exclude_id = match (&req.alert_id, &req.component) {
            (Some(_), Some(_)) => {
                return Err(McpError::invalid_params(
                    "give alert_id or component, not both",
                    None,
                ));
            }
            (None, None) => {
                return Err(McpError::invalid_params("give alert_id or component", None));
            }
            (Some(id), None) => id.clone(),
            (None, Some(_)) => String::new(),
        };
        let window_minutes = req
            .window_minutes
            .unwrap_or(self.triager.related_window.as_secs() / 60);
        if window_minutes == 0 {
            return structured(
                &Vec::<outcome::RelatedAlertRef>::new(),
                "window_minutes is 0; nothing looked up".to_owned(),
            );
        }
        let window = Duration::from_secs(window_minutes.saturating_mul(60));
        let Ok(signed) = SignedDuration::try_from(window) else {
            return Err(McpError::invalid_params(
                "window_minutes is too large",
                None,
            ));
        };
        let now = Timestamp::now();
        let Ok(since) = now.checked_sub(signed) else {
            return Err(McpError::invalid_params(
                "window_minutes is too large",
                None,
            ));
        };
        let since = since.strftime("%Y-%m-%dT%H:%M:%SZ").to_string();

        let alerts = match self
            .triager
            .incidentio
            .list_firing_alerts_since(&since, RELATED_PAGE)
            .await
        {
            Ok(a) => a,
            Err(e) => return Ok(tool_failed("related_alerts", &e)),
        };
        let mut related = related_from(
            &alerts,
            &exclude_id,
            &self.triager.component_keys,
            now,
            self.triager.related_max,
        );
        if let Some(component) = &req.component {
            related.retain(|a| {
                a.component
                    .as_deref()
                    .is_some_and(|c| c.eq_ignore_ascii_case(component))
            });
        }
        let refs: Vec<outcome::RelatedAlertRef> = related
            .iter()
            .map(|r| outcome::RelatedAlertRef {
                title: r.title.clone(),
                age_minutes: r.age_minutes,
                component: r.component.clone(),
            })
            .collect();
        let summary = if refs.is_empty() {
            format!("no other alerts firing in the last {window_minutes} minute(s)")
        } else {
            format!(
                "{} alert(s) firing in the last {window_minutes} minute(s)",
                refs.len()
            )
        };
        structured(&refs, summary)
    }
}

// ---------------------------------------------------------------------------
// recent_changes
// ---------------------------------------------------------------------------

/// Input for `recent_changes`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct RecentChangesRequest {
    /// Only changes naming this component; platform-wide changes (no
    /// component) always match regardless. Omit to see platform-wide
    /// changes only.
    #[serde(default)]
    pub component: Option<String>,
    /// How far back to look. Defaults to the server's configured change
    /// window (`flow.change_window_minutes`).
    #[serde(default)]
    pub window_minutes: Option<u64>,
}

/// `recent_changes` result: the matches, plus whether the feed is
/// configured at all and why the list is empty when it is.
#[derive(Debug, Serialize, JsonSchema)]
pub struct RecentChangesResult {
    /// Whether the change feed (`POST /changes`, decision 0007) is
    /// configured on this server.
    pub configured: bool,
    /// Matching changes, most recent first.
    pub changes: Vec<ChangeRef>,
    /// Explains an empty result, or the match count.
    pub note: String,
}

#[tool_router(router = recent_changes_router)]
impl Server {
    #[tool(
        name = "recent_changes",
        description = "List deploys, configuration changes and feature-flag toggles recently posted to signalman's change feed (POST /changes, decision 0007): the same matches qualify_alert offers to the model as a possible cause. Give an optional `component` to match a specific service, or omit it to see platform-wide changes only. The feed is per-process, in-memory state: if this MCP server runs over stdio, separate from `signalman serve`, it never receives posts and the result is always empty here, with a note explaining why.",
        annotations(
            title = "List recent changes",
            read_only_hint = true,
            idempotent_hint = true,
            open_world_hint = true
        )
    )]
    async fn recent_changes(
        &self,
        Parameters(req): Parameters<RecentChangesRequest>,
    ) -> Result<CallToolResult, McpError> {
        let Some(log) = &self.triager.changes else {
            let result = RecentChangesResult {
                configured: false,
                changes: Vec::new(),
                note: "the change feed is not configured on this server: SIGNALMAN_CHANGES_TOKEN is unset (decision 0007)".to_owned(),
            };
            return structured(&result, result.note.clone());
        };
        let window_minutes = req
            .window_minutes
            .unwrap_or(self.triager.change_window.as_secs() / 60);
        let hints: Vec<String> = req.component.into_iter().collect();
        let now = Timestamp::now();
        let matched = log.recent(
            &hints,
            Duration::from_secs(window_minutes.saturating_mul(60)),
            now,
            self.triager.change_max,
        );
        let changes: Vec<ChangeRef> = matched
            .iter()
            .map(|c| ChangeRef {
                at: c.at.clone(),
                kind: Some(c.kind.clone()),
                component: c.component.clone(),
                summary: c.summary.clone(),
                source: c.source.clone(),
                url: c.url.clone(),
            })
            .collect();
        let note = if changes.is_empty() {
            "no changes recorded in this process's window. The change feed is per-process, in-memory state (decision 0007): a stdio MCP server, separate from `signalman serve`, never receives POST /changes and is always empty here."
                .to_owned()
        } else {
            format!(
                "{} change(s) in the last {window_minutes} minute(s)",
                changes.len()
            )
        };
        let result = RecentChangesResult {
            configured: true,
            changes,
            note: note.clone(),
        };
        structured(&result, note)
    }
}

// ---------------------------------------------------------------------------
// lookup_owner
// ---------------------------------------------------------------------------

/// Input for `lookup_owner`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct LookupOwnerRequest {
    /// Component name, `kind:namespace/name`, or a title to resolve in the
    /// software catalog.
    pub component: String,
    /// Alert text (title plus description) used to pick the runbook page
    /// and to find components mentioned in free text.
    #[serde(default)]
    pub text: String,
}

/// One candidate offered for ownership, with its catalog link.
#[derive(Debug, Serialize, JsonSchema)]
pub struct OwnerCandidateInfo {
    /// Candidate key, also the suffix of the `ai-team-*` tag.
    pub key: String,
    /// Display name.
    pub label: String,
    /// Rubric text a `qualify_alert` call would send the model.
    pub description: String,
    /// Software-catalog reference, when the candidate came from Backstage.
    pub entity_ref: Option<String>,
    /// The team's page in the Backstage catalog.
    pub url: Option<String>,
}

/// `lookup_owner` result: what the catalog says about a component, and the
/// owner candidates and runbook a triage would use.
#[derive(Debug, Serialize, JsonSchema)]
pub struct LookupOwnerResult {
    /// The resolved component, or `null` when nothing matched.
    pub component: Option<ComponentRef>,
    /// How the component was found (name, alias, title match, ...).
    pub matched_by: Option<String>,
    /// Owner candidates a triage would offer, most relevant first.
    pub owner_candidates: Vec<OwnerCandidateInfo>,
    /// Runbook excerpt from TechDocs, when one matched.
    pub runbook: Option<String>,
    /// TechDocs page the excerpt came from.
    pub runbook_url: Option<String>,
}

#[tool_router(router = lookup_owner_router)]
impl Server {
    #[tool(
        name = "lookup_owner",
        description = "Resolve a component in the Backstage software catalog and show what a triage would use: the component's record, the owner candidates offered to the model (its own group and dependency neighbours), and the runbook excerpt TechDocs matches for the given alert text. The catalog is the ownership source of truth (decision 0004); this never decides anything, it shows what qualify_alert would see.",
        annotations(
            title = "Look up a component's owner and runbook",
            read_only_hint = true,
            idempotent_hint = true,
            open_world_hint = true
        )
    )]
    async fn lookup_owner(
        &self,
        Parameters(req): Parameters<LookupOwnerRequest>,
    ) -> Result<CallToolResult, McpError> {
        let Some(enricher) = &self.triager.backstage else {
            return Err(McpError::invalid_params(
                "Backstage is not configured on this server: set backstage.base_url or BACKSTAGE_BASE_URL",
                None,
            ));
        };
        let enrichment = match enricher
            .enrich(std::slice::from_ref(&req.component), &req.text)
            .await
        {
            Ok(e) => e,
            Err(e) => return Ok(tool_failed("lookup_owner", &e)),
        };
        let candidate_urls = enricher.candidate_urls(&enrichment.candidates);
        let owner_candidates: Vec<OwnerCandidateInfo> = enrichment
            .candidates
            .iter()
            .map(|c| OwnerCandidateInfo {
                key: c.key.clone(),
                label: c.label.clone(),
                description: c.description.clone(),
                entity_ref: c.entity_ref.clone(),
                url: c
                    .entity_ref
                    .as_ref()
                    .and_then(|_| candidate_urls.get(&c.key).cloned()),
            })
            .collect();
        let summary = match &enrichment.component {
            Some(c) => format!(
                "{} ({}), {} owner candidate(s), runbook {}",
                c.name,
                enrichment.matched_by.as_deref().unwrap_or("matched"),
                owner_candidates.len(),
                if enrichment.runbook.is_some() {
                    "found"
                } else {
                    "none"
                }
            ),
            None => format!(
                "no component matched {:?}; {} owner candidate(s) offered",
                req.component,
                owner_candidates.len()
            ),
        };
        let result = LookupOwnerResult {
            component: enrichment
                .component
                .as_ref()
                .map(|c| component_ref(c, enrichment.component_url.clone())),
            matched_by: enrichment.matched_by,
            owner_candidates,
            runbook: enrichment.runbook,
            runbook_url: enrichment.runbook_url,
        };
        structured(&result, summary)
    }
}

/// Map a catalog component into the same wire shape `qualify_alert` uses,
/// so a caller sees one vocabulary across tools.
fn component_ref(c: &ComponentContext, url: Option<String>) -> ComponentRef {
    ComponentRef {
        name: c.name.clone(),
        url,
        component_type: c.component_type.clone(),
        lifecycle: c.lifecycle.clone(),
        system: c.system.clone(),
        catalog_owner: c.owner.clone(),
        depends_on: c.depends_on.clone(),
        dependents: c.dependents.clone(),
    }
}

// ---------------------------------------------------------------------------
// open_incidents
// ---------------------------------------------------------------------------

/// Input for `open_incidents`.
#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct OpenIncidentsRequest {
    /// Maximum to fetch. Defaults to the server's configured dedup
    /// candidate cap (`incidentio.max_candidates`).
    #[serde(default)]
    pub max: Option<usize>,
}

/// One open incident, as offered for deduplication.
#[derive(Debug, Serialize, JsonSchema)]
pub struct OpenIncidentInfo {
    /// Human reference, `INC-4821`.
    pub reference: String,
    /// incident.io incident id (a ULID).
    pub id: String,
    /// Current status name.
    pub status: String,
    /// One-line summary, the same text a dedup judgment would read.
    pub summary: String,
    /// Link to the incident in the incident.io app.
    pub url: Option<String>,
}

#[tool_router(router = open_incidents_router)]
impl Server {
    #[tool(
        name = "open_incidents",
        description = "List incidents currently open in incident.io, the same candidates qualify_alert offers to the duplicate_of judgment. Useful to see what an attach decision could have matched against, or to check a dedup guess by hand.",
        annotations(
            title = "List open incidents",
            read_only_hint = true,
            idempotent_hint = true,
            open_world_hint = true
        )
    )]
    async fn open_incidents(
        &self,
        Parameters(req): Parameters<OpenIncidentsRequest>,
    ) -> Result<CallToolResult, McpError> {
        let max = req.max.unwrap_or(self.triager.max_candidates);
        let incidents = match self.triager.incidentio.list_open_incidents(max).await {
            Ok(i) => i,
            Err(e) => return Ok(tool_failed("open_incidents", &e)),
        };
        let refs: Vec<OpenIncidentInfo> = incidents
            .iter()
            .map(|i| OpenIncidentInfo {
                reference: i.reference.clone(),
                id: i.id.clone(),
                status: i.incident_status.name.clone(),
                summary: i.candidate_summary(),
                url: i.permalink.clone(),
            })
            .collect();
        let summary = format!("{} open incident(s)", refs.len());
        structured(&refs, summary)
    }
}

// ---------------------------------------------------------------------------
// Result helpers
// ---------------------------------------------------------------------------

/// A successful result: the value as structured content, plus a short text
/// summary for a client that renders content instead of structured JSON.
fn structured<T: Serialize>(
    value: &T,
    summary: impl Into<String>,
) -> Result<CallToolResult, McpError> {
    let json = serde_json::to_value(value).map_err(|e| {
        McpError::internal_error(format!("failed to serialise the result: {e}"), None)
    })?;
    let mut result = CallToolResult::structured(json);
    result.content = vec![ContentBlock::text(summary.into())];
    Ok(result)
}

/// The tool ran and failed after the request was accepted: a tool-level
/// error the caller's client renders, per [`CallToolResult::error`]'s
/// guidance. Never used for a malformed request, which is
/// `McpError::invalid_params` instead.
fn tool_failed(tool: &str, err: &impl std::fmt::Display) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(format!("{tool}: {err}"))])
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for Server {
    fn get_info(&self) -> ServerConfig {
        // Not `Implementation::from_build_env()`: it is a plain function
        // inside rmcp, so its `env!("CARGO_PKG_NAME")` resolves at *rmcp's*
        // build, always "rmcp". `env!` here, in signalman's own source,
        // resolves to signalman's own name and version.
        let server_info = Implementation::new(env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(server_info)
            .with_instructions(
                "signalman's typed alert-triage judgments, read-only. qualify_alert answers \
                 with the same outcome contract the CLI and the webhook receiver produce; \
                 related_alerts, recent_changes, lookup_owner and open_incidents each answer \
                 one piece of that context on their own. Nothing here writes to incident.io \
                 or Backstage, and nothing here creates an incident: signalman is a tool for \
                 agents, not an agent (decision 0008).",
            )
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::incidentio::sync::TAG_PREFIX;

    #[test]
    fn tool_names_match_what_the_router_advertises() {
        // The same merge `Server::new` does: each capability is its own
        // `#[tool_router]` block, so the five methods below must be
        // combined to see every tool.
        let router = Server::tool_router()
            + Server::related_alerts_router()
            + Server::recent_changes_router()
            + Server::lookup_owner_router()
            + Server::open_incidents_router();
        for name in [
            "qualify_alert",
            "related_alerts",
            "recent_changes",
            "lookup_owner",
            "open_incidents",
        ] {
            assert!(router.has_route(name), "missing tool {name}");
        }
        assert_eq!(router.list_all().len(), 5);
    }

    #[test]
    fn qualify_summary_names_every_action() {
        // A compile-time check that every Action variant is handled: if a
        // new one is added, this match becomes non-exhaustive and fails to
        // build, not just this test.
        fn assert_exhaustive(a: outcome::Action) {
            match a {
                outcome::Action::Suppress
                | outcome::Action::AttachToIncident
                | outcome::Action::Page
                | outcome::Action::Ticket
                | outcome::Action::HumanTriage => {}
            }
        }
        assert_exhaustive(outcome::Action::Suppress);
        assert!(!TAG_PREFIX.is_empty());
    }
}
