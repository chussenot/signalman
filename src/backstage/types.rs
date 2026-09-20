//! Catalog entity model, kept lenient: `spec` stays raw JSON and typed
//! accessors read the well-known fields; unknown fields are ignored.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Default namespace when a reference omits one.
pub const DEFAULT_NAMESPACE: &str = "default";

/// A parsed `kind:namespace/name` reference.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EntityRef {
    /// Lowercase kind, e.g. `component`, `group`.
    pub kind: String,
    /// Namespace, `default` when omitted.
    pub namespace: String,
    /// Entity name.
    pub name: String,
}

impl EntityRef {
    /// Parse `kind:namespace/name`, `kind:name`, `namespace/name` or `name`.
    /// `default_kind` fills in a missing kind, as the catalog does for
    /// `spec.owner` (group) and `spec.dependsOn` (component).
    pub fn parse(raw: &str, default_kind: &str) -> Option<Self> {
        let raw = raw.trim();
        if raw.is_empty() || raw.chars().any(char::is_whitespace) {
            return None;
        }
        let (kind, rest) = match raw.split_once(':') {
            Some((k, r)) => (k.to_ascii_lowercase(), r),
            None => (default_kind.to_ascii_lowercase(), raw),
        };
        let (namespace, name) = match rest.split_once('/') {
            Some((ns, n)) => (ns.to_ascii_lowercase(), n),
            None => (DEFAULT_NAMESPACE.to_owned(), rest),
        };
        if kind.is_empty() || namespace.is_empty() || name.is_empty() {
            return None;
        }
        Some(Self {
            kind,
            namespace,
            name: name.to_ascii_lowercase(),
        })
    }

    /// Build from parts.
    pub fn new(kind: &str, namespace: &str, name: &str) -> Self {
        Self {
            kind: kind.to_ascii_lowercase(),
            namespace: namespace.to_ascii_lowercase(),
            name: name.to_ascii_lowercase(),
        }
    }
}

impl fmt::Display for EntityRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}/{}", self.kind, self.namespace, self.name)
    }
}

/// A catalog entity as returned by the API. `spec` is raw; use the accessors.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entity {
    /// Kind, as written in the catalog (`Component`, `Group`, …).
    pub kind: String,
    /// Metadata block.
    pub metadata: Metadata,
    /// Raw spec.
    #[serde(default)]
    pub spec: Value,
    /// Relations the catalog derived from the spec, in both directions.
    #[serde(default)]
    pub relations: Vec<Relation>,
}

/// Entity metadata subset.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Metadata {
    /// Name, unique per kind and namespace.
    pub name: String,
    /// Namespace; the API always fills it in.
    #[serde(default)]
    pub namespace: Option<String>,
    /// Display title.
    #[serde(default)]
    pub title: Option<String>,
    /// Free-text description.
    #[serde(default)]
    pub description: Option<String>,
    /// Tags.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Annotations, including the well-known `backstage.io/*` ones.
    #[serde(default)]
    pub annotations: BTreeMap<String, String>,
    /// Links.
    #[serde(default)]
    pub links: Vec<Link>,
}

/// A link on an entity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Link {
    /// Target URL.
    pub url: String,
    /// Display title.
    #[serde(default)]
    pub title: Option<String>,
    /// Link type hint (e.g. `runbook`, `dashboard`).
    #[serde(default, rename = "type")]
    pub link_type: Option<String>,
}

/// A derived relation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Relation {
    /// Relation type, e.g. `ownedBy`, `dependsOn`, `dependencyOf`, `ownerOf`.
    #[serde(rename = "type")]
    pub relation_type: String,
    /// Target entity reference.
    #[serde(rename = "targetRef")]
    pub target_ref: String,
}

impl Entity {
    /// Namespace, defaulting to `default`.
    pub fn namespace(&self) -> &str {
        self.metadata
            .namespace
            .as_deref()
            .unwrap_or(DEFAULT_NAMESPACE)
    }

    /// This entity's reference.
    pub fn entity_ref(&self) -> EntityRef {
        EntityRef::new(&self.kind, self.namespace(), &self.metadata.name)
    }

    /// Best human label: `spec.profile.displayName` (groups and users), then
    /// `metadata.title`, then the name.
    pub fn label(&self) -> &str {
        self.profile_display_name()
            .filter(|d| !d.trim().is_empty())
            .unwrap_or_else(|| self.display_name())
    }

    /// Title if set, else name.
    pub fn display_name(&self) -> &str {
        self.metadata
            .title
            .as_deref()
            .filter(|t| !t.trim().is_empty())
            .unwrap_or(&self.metadata.name)
    }

    fn spec_str(&self, key: &str) -> Option<&str> {
        self.spec.get(key).and_then(Value::as_str)
    }

    /// `spec.type`.
    pub fn spec_type(&self) -> Option<&str> {
        self.spec_str("type")
    }

    /// `spec.lifecycle`.
    pub fn lifecycle(&self) -> Option<&str> {
        self.spec_str("lifecycle")
    }

    /// `spec.system`.
    pub fn system(&self) -> Option<EntityRef> {
        self.spec_str("system")
            .and_then(|s| EntityRef::parse(s, "system"))
    }

    /// `spec.profile.displayName` for groups and users.
    pub fn profile_display_name(&self) -> Option<&str> {
        self.spec
            .get("profile")
            .and_then(|p| p.get("displayName"))
            .and_then(Value::as_str)
    }

    /// Targets of relations of the given type.
    pub fn related<'a>(&'a self, relation_type: &'a str) -> impl Iterator<Item = EntityRef> + 'a {
        self.relations
            .iter()
            .filter(move |r| r.relation_type == relation_type)
            .filter_map(|r| EntityRef::parse(&r.target_ref, "component"))
    }

    /// The owner group from the `ownedBy` relation, or `spec.owner`.
    pub fn owner(&self) -> Option<EntityRef> {
        self.related("ownedBy").next().or_else(|| {
            self.spec_str("owner")
                .and_then(|o| EntityRef::parse(o, "group"))
        })
    }

    /// Entity that carries this entity's TechDocs, honouring
    /// `backstage.io/techdocs-entity`; `None` when no TechDocs are declared.
    pub fn techdocs_entity(&self) -> Option<EntityRef> {
        if let Some(target) = self
            .metadata
            .annotations
            .get("backstage.io/techdocs-entity")
        {
            return EntityRef::parse(target, "component");
        }
        self.metadata
            .annotations
            .contains_key("backstage.io/techdocs-ref")
            .then(|| self.entity_ref())
    }

    /// `backstage.io/view-url` annotation, if present.
    pub fn view_url(&self) -> Option<&str> {
        self.metadata
            .annotations
            .get("backstage.io/view-url")
            .map(String::as_str)
    }
}

/// `GET /entities/by-query` response.
#[derive(Debug, Deserialize)]
pub(crate) struct QueryResponse {
    pub items: Vec<Entity>,
    #[serde(default, rename = "pageInfo")]
    pub page_info: Option<PageInfo>,
}

#[derive(Debug, Deserialize, Default)]
pub(crate) struct PageInfo {
    #[serde(default, rename = "nextCursor")]
    pub next_cursor: Option<String>,
}

/// `POST /entities/by-refs` response: same length and order as the request.
#[derive(Debug, Deserialize)]
pub(crate) struct ByRefsResponse {
    pub items: Vec<Option<Entity>>,
}

/// mkdocs `search/search_index.json`, which TechDocs publishes with the site.
#[derive(Debug, Clone, PartialEq, Deserialize, Default)]
pub struct SearchIndex {
    /// One entry per page section.
    #[serde(default)]
    pub docs: Vec<SearchDoc>,
}

/// One search-index entry: a page or a section of a page.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct SearchDoc {
    /// Path within the site, e.g. `runbooks/high-error-rate/`.
    pub location: String,
    /// Section or page title.
    pub title: String,
    /// Plain text content.
    #[serde(default)]
    pub text: String,
}

/// Severity levels of the Notifications plugin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NotificationSeverity {
    /// Critical.
    Critical,
    /// High.
    High,
    /// Normal.
    Normal,
    /// Low.
    Low,
}

/// Payload of `POST /notifications/notifications`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NotificationPayload {
    /// Short title.
    pub title: String,
    /// Longer text.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Where to go.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
    /// Severity.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub severity: Option<NotificationSeverity>,
    /// Topic, used for per-topic user settings.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub topic: Option<String>,
    /// Scope, used to update an existing notification instead of adding one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use serde_json::json;

    #[test]
    fn entity_ref_parsing_fills_defaults_and_lowercases() {
        assert_eq!(
            EntityRef::parse("team-a", "group").unwrap().to_string(),
            "group:default/team-a"
        );
        assert_eq!(
            EntityRef::parse("Component:Prod/Checkout-API", "x")
                .unwrap()
                .to_string(),
            "component:prod/checkout-api"
        );
        assert_eq!(
            EntityRef::parse("payments/db", "resource")
                .unwrap()
                .to_string(),
            "resource:payments/db"
        );
        assert!(EntityRef::parse("", "group").is_none());
        assert!(EntityRef::parse("has space", "group").is_none());
    }

    #[test]
    fn accessors_read_relations_and_spec() {
        let e: Entity = serde_json::from_value(json!({
            "apiVersion": "backstage.io/v1alpha1", "kind": "Component",
            "metadata": { "name": "checkout-api", "namespace": "default", "title": "Checkout API",
                          "annotations": { "backstage.io/techdocs-ref": "dir:." }, "unknown": 1 },
            "spec": { "type": "service", "lifecycle": "production", "owner": "group:default/payments", "system": "shop" },
            "relations": [
                { "type": "ownedBy", "targetRef": "group:default/payments" },
                { "type": "dependsOn", "targetRef": "resource:default/orders-db" },
                { "type": "dependencyOf", "targetRef": "component:default/web" }
            ]
        })).unwrap();
        assert_eq!(e.display_name(), "Checkout API");
        assert_eq!(e.owner().unwrap().to_string(), "group:default/payments");
        assert_eq!(e.lifecycle(), Some("production"));
        assert_eq!(e.system().unwrap().to_string(), "system:default/shop");
        assert_eq!(
            e.related("dependencyOf")
                .map(|r| r.to_string())
                .collect::<Vec<_>>(),
            vec!["component:default/web"]
        );
        assert_eq!(
            e.techdocs_entity().unwrap().to_string(),
            "component:default/checkout-api"
        );
    }
}
