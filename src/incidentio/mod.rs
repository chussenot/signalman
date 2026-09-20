//! incident.io integration.
//!
//! incident.io is used as the alert hub, not bypassed: alerts from any source
//! (Datadog, Alertmanager, ...) land in incident.io first. A
//! `public_alert.alert_created_v1` webhook then triggers this flow:
//!
//! 1. verify the Svix signature ([`webhook`]);
//! 2. fetch the alert's latest state and the live incidents from the API
//!    ([`client`]), as the webhook docs recommend, so ordering does not matter;
//! 3. run the TypeSafe fan-out and the routing policy ([`crate::triage`]);
//! 4. write the judgments back as alert tags, for a confident duplicate an
//!    attachment to the existing incident, and one qualification note the
//!    responder reads in the alert view ([`sync`], [`note`]).
//!
//! The CLI can also pull live incidents as dedup candidates and forward an
//! enriched alert to an HTTP alert source, letting incident.io's alert routes
//! decide escalation. Incidents are never created directly.

pub mod client;
pub mod error;
pub mod note;
pub mod sync;
pub mod types;
pub mod webhook;

pub use client::Client;
pub use error::{Error, Result};
pub use sync::{Outcome, Triager, WriteBack};
pub use types::{Alert, AlertEvent, AlertNote, AlertStatus, Incident, StatusCategory};
pub use webhook::{Event, WebhookSecret, verify};
