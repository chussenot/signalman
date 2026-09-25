//! # judgment
//!
//! Typed, calibrated judgments from [TypeSafe](https://docs.typesafe.ai) System
//! One models (Jev) and from any backend that speaks the same wire.
//!
//! ## The problem
//!
//! A generative model asked to classify something answers in prose, or in JSON
//! it was told to produce. A field named `confidence` in that JSON is generated
//! text, not a measured probability. The answer drifts between runs and between
//! prompt versions, a parse failure becomes a failure of the decision it was
//! meant to inform, and the decision itself is buried in the prose. None of
//! that can carry an action that pages someone at night.
//!
//! A System One model does not generate text. It evaluates a `state` (any JSON)
//! against typed questions and returns calibrated answers:
//!
//! | Primitive | Question | Answer |
//! |---|---|---|
//! | [`Noul`] | yes/no | probability of yes |
//! | [`Choice`] | one of a defined set | chosen option, full distribution, confidence |
//! | [`Score`] | degree on ordered levels | weighted position, per-level distribution, confidence |
//!
//! Calibrated means the probabilities can be measured against outcomes and
//! held to account ([`eval`] does the measuring). That lets code own the
//! workflow: the model supplies a number, and the threshold that turns the
//! number into an action is written, tested and tuned in code, with no new
//! inference when the threshold changes. The application this crate came from
//! chose calibrated judgments over generated text for that reason; this crate
//! is that choice made reusable.
//!
//! ## What this crate guarantees
//!
//! The wire is a map of `id -> question` going out and `id -> answer` coming
//! back, and nothing on it ties an answer to the type of the question it
//! answers. This crate closes that gap on the Rust side:
//!
//! * A question's handle fixes its answer's type. [`Questions::noul`],
//!   [`Questions::choice`] and [`Questions::score`] return a [`Handle`] whose
//!   type parameter is the answer type. [`Response::get`] checks the id and the
//!   primitive, and maps a typed Choice's option key back to the enum. A
//!   mismatch is an [`Error`], never a misread number.
//! * Probabilities are validated newtypes. [`Probability`] and [`Confidence`]
//!   refuse values outside `[0, 1]` on construction and on deserialisation,
//!   and they are distinct types, so a caller cannot threshold one as the
//!   other.
//! * Limits are checked before sending. 255 options per Choice, 2 to 10 levels
//!   per Score and unique ids are enforced by the builder, so a bad question is
//!   an error naming the question, not a 422 after a round trip.
//! * Retries follow the official SDKs' defaults and say, on [`RetryPolicy`],
//!   where they differ. Behaviour matches across languages, the worst case
//!   is bounded, and the reasoning behind each field is on that type.
//!
//! ## What it deliberately is not
//!
//! * Not a sync client, and no batching or streaming. The API documents one
//!   evaluation endpoint (and a model listing) and one request shape, and one
//!   request already carries many questions. A caller that must block can
//!   block on the future.
//! * Not a metrics backend. A library must not choose one for the application
//!   that embeds it. The crate emits `tracing` spans and hands token usage and
//!   failed attempts to an [`Observer`]; the application counts them where it
//!   counts everything else.
//! * Not tied to the hosted model. [`SystemOne`] is a one-method trait, and
//!   the code that consumes judgments never knows whether they came from the
//!   hosted model, a [`Fake`] in a test or a [`Replay`] of recorded answers.
//!   Should TypeSafe publish an official Rust SDK, it slots in as one more
//!   backend behind the same trait.
//!
//! ## Walkthrough
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
//! ## Modules
//!
//! * [`question`] builds requests; each question returns a typed [`Handle`].
//! * [`answer`] validates probabilities and converts wire answers into typed
//!   views through those handles.
//! * [`client`] (feature `http`, on by default) talks HTTP with the SDKs'
//!   defaults, retries and errors; the differences are stated on
//!   [`RetryPolicy`]. [`Client::evaluate_with`] takes a [`CallOptions`] for
//!   one call's timeout, retry policy, headers and extra body fields, and
//!   refuses any that would replace what the client sets itself.
//! * [`http`] (feature `http`) is the retry loop and
//!   [`RetryPolicy`], public so another client over
//!   `reqwest` can share them.
//! * [`error`] is one enum, grouped by what fixes each variant.
//! * [`backend`] is where answers come from: [`SystemOne`] is the trait,
//!   [`Client`] one implementation, [`Fake`],
//!   [`Recorder`] and [`Replay`] the others.
//! * [`eval`] measures: recordings for replay, graded judgments, accuracy,
//!   Brier score and calibration error per question.
//! * [`observer`] is the seam an application uses to count tokens and failed
//!   attempts in its own metrics.
//!
//! Without the `http` feature the crate is the question builder, the typed
//! answers, the backends other than the client, and the recordings and
//! metrics, for a project that brings its own transport.

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
pub use client::{
    CallOptions, Client, ClientBuilder, ModelInfo, Request, RetryPolicy, TransportRetry,
};
pub use error::{Error, Result, ValidationIssue};
pub use observer::Observer;
pub use question::{Handle, NoulCriteria, Options, Question, Questions};
