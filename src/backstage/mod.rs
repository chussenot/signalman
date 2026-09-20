//! Backstage bridge.
//!
//! The software catalog is the ownership source of truth. For an alert,
//! signalman resolves the alerting component, reads its owner group and its
//! dependency graph, and offers the resulting groups to the model as the
//! owner candidates instead of a hard-coded team list. TechDocs supplies the
//! runbook excerpt, and the Notifications plugin tells the owning group what
//! was decided.
//!
//! Contracts used (Backstage backend REST, all under `{base}/api/…`):
//!
//! | Plugin | Endpoint |
//! |---|---|
//! | catalog | `GET /catalog/entities/by-name/{kind}/{namespace}/{name}` |
//! | catalog | `GET /catalog/entities/by-query?filter=…&fields=…&limit=…&cursor=…` |
//! | catalog | `POST /catalog/entities/by-refs` |
//! | techdocs | `GET /techdocs/static/docs/{namespace}/{kind}/{name}/search/search_index.json` |
//! | notifications | `POST /notifications/notifications` |
//!
//! Authentication is a static external-access token
//! (`backend.auth.externalAccess`, type `static`) sent as a bearer token.

pub mod client;
pub mod enrich;
pub mod error;
pub mod types;

pub use client::Client;
pub use enrich::{Enricher, Enrichment};
pub use error::{Error, Result};
pub use types::{Entity, EntityRef, NotificationPayload, NotificationSeverity};
