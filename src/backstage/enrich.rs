//! Turn catalog data into triage context: the alerting component, its owner
//! group and neighbours as owner candidates, and a TechDocs runbook excerpt.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use super::client::Client;
use super::error::Result;
use super::types::{
    Entity, EntityRef, NotificationPayload, NotificationSeverity, SearchDoc, SearchIndex,
};
use crate::triage::{ComponentContext, Decision, Impact, OwnerCandidate, OwnerCandidates};

/// Default cap on owner candidates offered to the model.
pub const DEFAULT_MAX_CANDIDATES: usize = 24;

/// Default cap on the runbook excerpt.
pub const DEFAULT_RUNBOOK_CHARS: usize = 1_800;

/// Resolves alerts against the catalog.
#[derive(Debug, Clone)]
pub struct Enricher {
    /// Backstage client.
    pub client: Client,
    /// Namespace tried first when a hint has none.
    pub namespace: String,
    /// Cap on owner candidates (the Choice limit is 255; tokens cost more).
    pub max_candidates: usize,
    /// Cap on the runbook excerpt length.
    pub runbook_chars: usize,
    /// Frontend base URL for links in notes; the backend URL when the app is
    /// served from the same host (`backstage.app_url` in the configuration).
    pub app_url: String,
}

/// What the catalog contributed for one alert.
#[derive(Debug, Clone, Serialize)]
pub struct Enrichment {
    /// The resolved component, when a hint matched.
    pub component: Option<ComponentContext>,
    /// Owner candidates for the owner question.
    #[serde(skip)]
    pub candidates: OwnerCandidates,
    /// Runbook excerpt from TechDocs.
    pub runbook: Option<String>,
    /// TechDocs page the excerpt came from.
    pub runbook_url: Option<String>,
    /// Backstage page of the resolved component.
    pub component_url: Option<String>,
    /// How the component was found, for logs.
    pub matched_by: Option<String>,
}

impl Enricher {
    /// Build with defaults: namespace `default`, links on the backend URL.
    /// The configuration layer sets the rest.
    pub fn new(client: Client) -> Self {
        let app_url = client.base_url().as_str().trim_end_matches('/').to_owned();
        Self {
            client,
            namespace: "default".into(),
            max_candidates: DEFAULT_MAX_CANDIDATES,
            runbook_chars: DEFAULT_RUNBOOK_CHARS,
            app_url,
        }
    }

    /// Frontend URL for links, without a trailing slash.
    #[must_use]
    pub fn with_app_url(mut self, app_url: impl Into<String>) -> Self {
        let app_url: String = app_url.into();
        app_url.trim_end_matches('/').clone_into(&mut self.app_url);
        self
    }

    /// Namespace tried first for bare component names.
    #[must_use]
    pub fn with_namespace(mut self, namespace: impl Into<String>) -> Self {
        self.namespace = namespace.into();
        self
    }

    /// Frontend page of an entity: `{app_url}/catalog/{namespace}/{kind}/{name}`.
    pub fn entity_url(&self, r: &EntityRef) -> String {
        format!(
            "{}/catalog/{}/{}/{}",
            self.app_url,
            r.namespace,
            r.kind.to_ascii_lowercase(),
            r.name
        )
    }

    /// Catalog page of every owner candidate that carries an entity
    /// reference, keyed by candidate key.
    ///
    /// One place computes these links, so the webhook flow and the CLI
    /// cannot drift apart on what an owner's URL is.
    pub fn candidate_urls(&self, candidates: &OwnerCandidates) -> BTreeMap<String, String> {
        candidates
            .iter()
            .filter_map(|c| {
                let r = EntityRef::parse(c.entity_ref.as_deref()?, "group")?;
                Some((c.key.clone(), self.entity_url(&r)))
            })
            .collect()
    }

    /// TechDocs page: `{app_url}/docs/{namespace}/{kind}/{name}/{location}`.
    pub fn techdocs_url(&self, r: &EntityRef, location: &str) -> String {
        format!(
            "{}/docs/{}/{}/{}/{}",
            self.app_url,
            r.namespace,
            r.kind.to_ascii_lowercase(),
            r.name,
            location.trim_start_matches('/')
        )
    }

    /// Resolve `hints` (service names, component names or entity refs) to a
    /// component, then assemble context, owner candidates and runbook.
    ///
    /// `alert_text` (title plus description) selects the runbook page and
    /// finds components mentioned by name. Never fails on a missing entity;
    /// only transport and auth errors propagate.
    #[allow(clippy::too_many_lines)] // one pass over the graph; splitting hides the data flow
    #[tracing::instrument(name = "backstage.enrich", skip_all, fields(hints = ?hints))]
    pub async fn enrich(&self, hints: &[String], alert_text: &str) -> Result<Enrichment> {
        let (component, matched_by) = self.resolve_component(hints).await?;

        let Some(component) = component else {
            let candidates = self.all_team_candidates().await?;
            return Ok(Enrichment {
                component: None,
                candidates,
                runbook: None,
                runbook_url: None,
                component_url: None,
                matched_by: None,
            });
        };

        // Neighbours: what it depends on and what depends on it.
        let dep_refs: Vec<EntityRef> = component
            .related("dependsOn")
            .chain(component.related("dependencyOf"))
            .chain(component.related("consumesApi"))
            .chain(component.related("providesApi"))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let neighbours = self
            .client
            .get_by_refs(
                &dep_refs,
                Some(&[
                    "kind",
                    "metadata.name",
                    "metadata.namespace",
                    "metadata.title",
                    "relations",
                ]),
            )
            .await?;

        // Owner groups: the component's, its neighbours', and those of any
        // component whose name appears in the alert text.
        let mut group_refs: Vec<EntityRef> = Vec::new();
        let mut push = |r: Option<EntityRef>| {
            if let Some(r) = r
                && r.kind == "group"
                && !group_refs.contains(&r)
            {
                group_refs.push(r);
            }
        };
        push(component.owner());
        for n in &neighbours {
            push(n.owner());
        }
        for m in self.components_mentioned(alert_text, &component).await? {
            push(m.owner());
        }
        let groups = self
            .client
            .get_by_refs(
                &group_refs,
                Some(&["kind", "metadata", "spec.profile", "spec.type", "relations"]),
            )
            .await?;
        let mut candidates = self.candidates_from_groups(&groups);
        if candidates.is_empty() {
            candidates = self.all_team_candidates().await?;
        }

        let (runbook, runbook_url) = self
            .runbook_for(&component, alert_text)
            .await?
            .map_or((None, None), |(text, url)| (Some(text), Some(url)));
        let component_url = Some(self.entity_url(&component.entity_ref()));

        let owner_group = groups
            .iter()
            .find(|g| Some(g.entity_ref()) == component.owner());
        let context = ComponentContext {
            name: component.metadata.name.clone(),
            title: component.metadata.title.clone(),
            description: component.metadata.description.clone(),
            component_type: component.spec_type().map(str::to_owned),
            lifecycle: component.lifecycle().map(str::to_owned),
            system: component.system().map(|s| s.name),
            owner: owner_group
                .map(|g| g.label().to_owned())
                .or_else(|| component.owner().map(|o| o.name)),
            owner_description: owner_group.and_then(|g| g.metadata.description.clone()),
            depends_on: neighbours
                .iter()
                .filter(|n| {
                    component.related("dependsOn").any(|r| r == n.entity_ref())
                        || component
                            .related("consumesApi")
                            .any(|r| r == n.entity_ref())
                })
                .map(|n| format!("{} {}", n.kind.to_ascii_lowercase(), n.metadata.name))
                .collect(),
            dependents: neighbours
                .iter()
                .filter(|n| {
                    component
                        .related("dependencyOf")
                        .any(|r| r == n.entity_ref())
                        || component
                            .related("providesApi")
                            .any(|r| r == n.entity_ref())
                })
                .map(|n| format!("{} {}", n.kind.to_ascii_lowercase(), n.metadata.name))
                .collect(),
            tags: component.metadata.tags.clone(),
            links: component
                .metadata
                .links
                .iter()
                .map(|l| l.title.clone().unwrap_or_else(|| l.url.clone()))
                .collect(),
        };

        Ok(Enrichment {
            component: Some(context),
            candidates,
            runbook,
            runbook_url,
            component_url,
            matched_by,
        })
    }

    async fn resolve_component(
        &self,
        hints: &[String],
    ) -> Result<(Option<Entity>, Option<String>)> {
        for hint in hints.iter().map(|h| h.trim()).filter(|h| !h.is_empty()) {
            // Full or partial entity reference, or a bare name.
            let candidates_refs = if hint.contains(':') || hint.contains('/') {
                EntityRef::parse(hint, "component")
                    .into_iter()
                    .collect::<Vec<_>>()
            } else {
                let mut v = vec![EntityRef::new("component", &self.namespace, hint)];
                if self.namespace != "default" {
                    v.push(EntityRef::new("component", "default", hint));
                }
                v
            };
            for r in candidates_refs {
                if let Some(e) = self.client.get_by_name(&r).await? {
                    return Ok((Some(e), Some(format!("name {hint}"))));
                }
            }
            // Title match.
            let by_title = self
                .client
                .query(
                    &[&[("kind", Some("component")), ("metadata.title", Some(hint))]],
                    None,
                    1,
                )
                .await?;
            if let Some(e) = by_title.into_iter().next() {
                return Ok((Some(e), Some(format!("title {hint}"))));
            }
        }
        Ok((None, None))
    }

    /// Components whose name appears as a word in the alert text (other than
    /// the resolved one). Bounded by a single query on the candidate names.
    async fn components_mentioned(&self, text: &str, resolved: &Entity) -> Result<Vec<Entity>> {
        let words: BTreeSet<String> = text
            .split(|c: char| !(c.is_alphanumeric() || c == '-' || c == '_'))
            .filter(|w| w.len() >= 4 && w.contains(|c: char| c.is_alphabetic()))
            .map(str::to_ascii_lowercase)
            .filter(|w| *w != resolved.metadata.name)
            .take(40)
            .collect();
        if words.is_empty() {
            return Ok(Vec::new());
        }
        let refs: Vec<EntityRef> = words
            .iter()
            .map(|w| EntityRef::new("component", &self.namespace, w))
            .collect();
        self.client
            .get_by_refs(
                &refs,
                Some(&["kind", "metadata.name", "metadata.namespace", "relations"]),
            )
            .await
    }

    async fn all_team_candidates(&self) -> Result<OwnerCandidates> {
        let groups = self
            .client
            .query(
                &[&[("kind", Some("group")), ("spec.type", Some("team"))]],
                Some("kind,metadata,spec.profile,spec.type,relations"),
                self.max_candidates,
            )
            .await?;
        Ok(self.candidates_from_groups(&groups))
    }

    fn candidates_from_groups(&self, groups: &[Entity]) -> OwnerCandidates {
        let mut list: Vec<OwnerCandidate> = groups
            .iter()
            .take(self.max_candidates)
            .map(|g| {
                let owned: Vec<String> = g
                    .related("ownerOf")
                    .filter(|r| r.kind == "component")
                    .map(|r| r.name)
                    .take(6)
                    .collect();
                let mut description = g.label().to_owned();
                if let Some(d) = g
                    .metadata
                    .description
                    .as_deref()
                    .filter(|d| !d.trim().is_empty())
                {
                    description.push_str(": ");
                    description.push_str(d.trim());
                }
                if !owned.is_empty() {
                    description.push_str(". Owns: ");
                    description.push_str(&owned.join(", "));
                }
                OwnerCandidate {
                    key: g.metadata.name.clone(),
                    label: g.label().to_owned(),
                    description,
                    entity_ref: Some(g.entity_ref().to_string()),
                }
            })
            .collect();
        list.sort_by(|a, b| a.key.cmp(&b.key));
        list.dedup_by(|a, b| a.key == b.key);
        OwnerCandidates::new(list)
    }

    /// Runbook excerpt and the TechDocs page it came from.
    async fn runbook_for(
        &self,
        component: &Entity,
        alert_text: &str,
    ) -> Result<Option<(String, String)>> {
        let Some(docs_ref) = component.techdocs_entity() else {
            return Ok(None);
        };
        let Some(index) = self.client.techdocs_search_index(&docs_ref).await? else {
            return Ok(None);
        };
        Ok(select_runbook_doc(&index, alert_text).map(|d| {
            (
                excerpt(d, self.runbook_chars),
                self.techdocs_url(&docs_ref, &d.location),
            )
        }))
    }

    /// Tell the owning group what was decided. Only for decisions that need
    /// a person; suppressions and attachments stay silent.
    #[tracing::instrument(name = "backstage.notify_owner", skip_all, fields(owner = %owner.key))]
    pub async fn notify_owner(
        &self,
        owner: &OwnerCandidate,
        alert_title: &str,
        decision: &Decision,
        link: Option<String>,
    ) -> Result<bool> {
        let Some(entity_ref) = owner
            .entity_ref
            .as_deref()
            .and_then(|r| EntityRef::parse(r, "group"))
        else {
            return Ok(false);
        };
        let (title, severity) = match decision {
            Decision::Page { impact, .. } => (
                format!("Page: {alert_title}"),
                if *impact >= Impact::Outage {
                    NotificationSeverity::Critical
                } else {
                    NotificationSeverity::High
                },
            ),
            Decision::Ticket { .. } => (
                format!("Ticket: {alert_title}"),
                NotificationSeverity::Normal,
            ),
            Decision::HumanTriage { .. } => (
                format!("Needs triage: {alert_title}"),
                NotificationSeverity::Normal,
            ),
            Decision::Suppress { .. } | Decision::AttachToIncident { .. } => return Ok(false),
        };
        let payload = NotificationPayload {
            title,
            description: Some(describe(decision)),
            link,
            severity: Some(severity),
            topic: Some("signalman".into()),
            scope: None,
        };
        self.client.notify_entity(&entity_ref, &payload).await?;
        Ok(true)
    }
}

/// The search-index entry that best matches the alert, if any scores above
/// the noise floor.
pub fn select_runbook_doc<'a>(index: &'a SearchIndex, alert_text: &str) -> Option<&'a SearchDoc> {
    let words: BTreeSet<String> = alert_text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() >= 3)
        .map(str::to_ascii_lowercase)
        .collect();
    index
        .docs
        .iter()
        .filter(|d| !d.text.trim().is_empty())
        .map(|d| {
            let loc = d.location.to_ascii_lowercase();
            let title = d.title.to_ascii_lowercase();
            let mut score: i64 = 0;
            if loc.contains("runbook") || title.contains("runbook") {
                score += 10;
            }
            for w in &words {
                if title.contains(w.as_str()) {
                    score += 4;
                }
                if loc.contains(w.as_str()) {
                    score += 2;
                }
            }
            // Top-level pages over anchored sections, all else equal.
            if !loc.contains('#') {
                score += 1;
            }
            (score, d)
        })
        .filter(|(s, _)| *s > 1)
        .max_by_key(|(s, d)| (*s, std::cmp::Reverse(d.location.len())))
        .map(|(_, d)| d)
}

/// `Title (location): text`, cut at `max_chars` on a character boundary.
fn excerpt(d: &SearchDoc, max_chars: usize) -> String {
    let mut text = format!(
        "{} ({}): {}",
        d.title.trim(),
        d.location.trim_end_matches('/'),
        d.text.trim()
    );
    if text.len() > max_chars {
        let cut = text.floor_char_boundary(max_chars);
        text.truncate(cut);
        text.push('…');
    }
    text
}

fn describe(decision: &Decision) -> String {
    match decision {
        Decision::Page {
            owner,
            impact,
            confirm_owner,
            suspected_change,
        } => format!(
            "signalman judged this alert {impact:?} impact and routed it to {}{}{}.",
            owner.label,
            if *confirm_owner {
                " (owner confidence was moderate; please confirm)"
            } else {
                ""
            },
            if *suspected_change {
                " A recent change is a suspected cause."
            } else {
                ""
            }
        ),
        Decision::Ticket { owner, impact, .. } => {
            format!(
                "signalman judged this alert {impact:?} impact; a ticket for {} rather than a page.",
                owner.label
            )
        }
        Decision::HumanTriage {
            best_guess,
            confidence,
            impact,
        } => format!(
            "signalman could not attribute this alert with confidence (best guess {}, confidence {confidence:.2}); impact judged {impact:?}. A person should triage.",
            best_guess.label
        ),
        Decision::Suppress { .. } | Decision::AttachToIncident { .. } => String::new(),
    }
}

/// Map incident.io alert attributes and CLI labels to component hints.
pub fn hints_from_labels(labels: &BTreeMap<String, String>, keys: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for key in keys {
        for (k, v) in labels {
            if k.eq_ignore_ascii_case(key) && !v.trim().is_empty() && !out.contains(v) {
                out.push(v.clone());
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::backstage::types::SearchDoc;

    fn doc(location: &str, title: &str, text: &str) -> SearchDoc {
        SearchDoc {
            location: location.into(),
            title: title.into(),
            text: text.into(),
        }
    }

    #[test]
    fn runbook_prefers_runbook_pages_matching_the_alert() {
        let index = SearchIndex {
            docs: vec![
                doc("", "Checkout API", "Overview of the service."),
                doc(
                    "runbooks/high-error-rate/",
                    "High error rate",
                    "Check the payments gateway first, then roll back.",
                ),
                doc(
                    "runbooks/high-error-rate/#steps",
                    "Steps",
                    "1. Look at dashboards",
                ),
                doc("adr/0001/", "Use Postgres", "We chose Postgres."),
            ],
        };
        let r = select_runbook_doc(&index, "HighErrorRate checkout-api 5xx ratio 12%")
            .map(|d| excerpt(d, 500))
            .unwrap();
        assert!(
            r.starts_with("High error rate (runbooks/high-error-rate): Check the payments gateway"),
            "{r}"
        );
        assert!(select_runbook_doc(&SearchIndex::default(), "x").is_none());
        // Weak matches are not returned.
        assert!(
            select_runbook_doc(
                &SearchIndex {
                    docs: vec![doc("adr/0001/", "Use Postgres", "text")]
                },
                "DiskUsageHigh",
            )
            .is_none()
        );
    }

    #[test]
    fn runbook_is_bounded() {
        let index = SearchIndex {
            docs: vec![doc("runbooks/x/", "Runbook", "a".repeat(5000).as_str())],
        };
        let r = select_runbook_doc(&index, "anything")
            .map(|d| excerpt(d, 200))
            .unwrap();
        assert!(r.chars().count() <= 201);
        assert!(r.ends_with('…'));
    }

    #[test]
    fn hints_follow_key_order_and_dedupe() {
        let labels: BTreeMap<String, String> = [
            ("Service", "checkout-api"),
            ("app", "checkout-api"),
            ("namespace", "shop"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect();
        assert_eq!(
            hints_from_labels(
                &labels,
                &crate::config::DEFAULT_COMPONENT_KEYS.map(String::from)
            ),
            vec!["checkout-api"]
        );
    }
}
