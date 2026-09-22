//! # signalman
//!
//! A typed Rust client for the [TypeSafe](https://docs.typesafe.ai) System One
//! API, plus a worked alert-triage application built on it.
//!
//! TypeSafe's model, Jev, does not generate text. It evaluates a `state` (any
//! JSON) against typed questions and returns calibrated judgments:
//!
//! | Primitive | Question | Answer |
//! |---|---|---|
//! | [`Noul`] | yes/no | probability of yes |
//! | [`Choice`] | one of a defined set | chosen option, full distribution, confidence |
//! | [`Score`] | degree on ordered levels | weighted position, per-level distribution, confidence |
//!
//! Code owns the workflow; the model supplies the judgment. This crate keeps
//! that boundary typed end to end:
//!
//! ```no_run
//! use signalman::{Client, Questions, options};
//!
//! options! {
//!     enum Department {
//!         Billing = "billing" => "Payments, invoicing, refunds",
//!         Technical = "technical" => "Bugs, outages, integrations",
//!         Sales = "sales" => "Pricing, upgrades, new accounts",
//!     }
//! }
//!
//! # async fn run() -> signalman::Result<()> {
//! let mut questions = Questions::new();
//! let dept = questions.choice::<Department>("department", "Which team should handle `message`?")?;
//! let urgent = questions.noul("is_urgent", "Does `message` convey urgency?", None)?;
//!
//! let client = Client::from_env()?;
//! let state = serde_json::json!({ "message": "Help! My payouts have been failing for 3 days." });
//! let response = client.system_one(&state, &questions).await?;
//!
//! let dept = response.get(&dept)?;          // Choice<Department>
//! let urgent = response.get(&urgent)?;      // Noul
//! if dept.chosen == Department::Billing && dept.confidence.at_least(0.7) && urgent.is_yes(0.6) {
//!     // page billing on-call
//! }
//! # Ok(()) }
//! ```
//!
//! * [`question`] builds requests; each question returns a typed [`Handle`].
//! * [`answer`] validates probabilities and converts wire answers into typed
//!   views through those handles.
//! * [`client`] talks HTTP with SDK-equivalent defaults, retries and errors.
//! * [`triage`] is the application: speculative fan-out over an alert, then a
//!   routing policy with risk-scaled confidence thresholds.
//! * [`backstage`] resolves the alerting component in the software catalog,
//!   offers its owner group and neighbours as owner candidates, pulls the
//!   runbook from TechDocs, and notifies the owning group.
//! * [`incidentio`] connects the triage to incident.io: an API client, Svix
//!   webhook verification, and the sync flow that triages an alert incident.io
//!   received and writes tags and incident attachments back.
//! * [`outcome`] is the JSON contract every triage emits, whichever way it
//!   was started: one document per alert, schema committed and drift-tested.
//! * [`serve`] is the webhook receiver.

pub mod answer;
pub mod backstage;
pub mod changes;
pub mod client;
pub mod config;
pub mod error;
pub mod eval;
pub mod http;
pub mod incidentio;
pub mod mcp;
pub mod outcome;
pub mod question;
pub mod serve;
pub mod triage;

pub use answer::{
    Answer, Choice, Confidence, FromAnswer, Noul, Probability, Response, Score, Usage,
};
pub use client::{Client, ClientBuilder, ModelInfo, Request, RetryPolicy};
pub use error::{Error, Result};
pub use outcome::Outcome;
pub use question::{Handle, NoulCriteria, Options, Question, Questions};
