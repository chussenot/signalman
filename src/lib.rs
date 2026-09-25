//! # signalman
//!
//! An alert-triage application built on [`judgment`], the typed client for
//! the [TypeSafe](https://docs.typesafe.ai) System One API that this crate
//! re-exports (decision 0010).
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
//! * [`question`], [`answer`] and [`client`] are [`judgment`]'s, re-exported:
//!   requests with typed handles, validated answers, retries on the official
//!   SDKs' defaults with the differences stated on [`RetryPolicy`].
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

pub mod backstage;
pub mod changes;
pub mod config;
pub mod eval;
pub mod incidentio;
pub mod mcp;
pub mod outcome;
pub mod readiness;
pub mod serve;
pub mod telemetry;
pub mod triage;

// The judgment core, re-exported so `signalman::Client`, `signalman::options!`
// and `crate::answer::..` keep working for this crate and its tests.
pub use judgment::{
    Answer, Choice, Client, ClientBuilder, Confidence, Error, FromAnswer, Handle, ModelInfo, Noul,
    NoulCriteria, Options, Probability, Question, Questions, Request, Response, Result,
    RetryPolicy, Score, Usage, answer, client, error, http, options, question,
};
pub use outcome::Outcome;
