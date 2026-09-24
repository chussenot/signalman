//! # judgment
//!
//! Typed, calibrated judgments from [TypeSafe](https://docs.typesafe.ai) System
//! One models (Jev) and backends that speak the same wire (Laya behind a shim).
//! Extracted from the signalman alert triager (decision 0010) so any Rust
//! project can build on the same layer without carrying that application.
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
//! use judgment::{Client, Questions, options};
//!
//! options! {
//!     enum Department {
//!         Billing = "billing" => "Payments, invoicing, refunds",
//!         Technical = "technical" => "Bugs, outages, integrations",
//!         Sales = "sales" => "Pricing, upgrades, new accounts",
//!     }
//! }
//!
//! # async fn run() -> judgment::Result<()> {
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
//! * [`backend`] is where answers come from: [`SystemOne`] is the trait,
//!   [`Client`] one implementation, [`Fake`], [`Recorder`] and [`Replay`]
//!   the others, so a test or an offline run never needs the model.
//! * [`eval`] measures: recordings for replay, graded judgments, accuracy,
//!   Brier score and calibration error per question.
//! * [`observer`] is the seam an application uses to count tokens and failed
//!   attempts in its own metrics; the crate itself only emits `tracing` spans.
//!
//! Without the default `http` feature the crate is the question builder and
//! the typed answers alone, for a project that brings its own transport.

pub mod answer;
pub mod backend;
#[cfg(feature = "http")]
pub mod client;
pub mod error;
pub mod eval;
#[cfg(feature = "http")]
pub mod http;
pub mod observer;
pub mod question;

pub use answer::{
    Answer, Choice, Confidence, FromAnswer, Noul, Probability, Response, Score, Usage,
};
pub use backend::{Fake, Recorder, Replay, SystemOne};
#[cfg(feature = "http")]
pub use client::{Client, ClientBuilder, ModelInfo, Request, RetryPolicy};
pub use error::{Error, Result};
pub use observer::Observer;
pub use question::{Handle, NoulCriteria, Options, Question, Questions};
