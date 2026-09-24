//! One error type for the client and the typed answer layer.
//!
//! A caller has to pick a remedy from the error alone, so the variants are
//! grouped by what fixes them rather than by HTTP status:
//!
//! * Configuration: [`Error::MissingApiKey`] and [`Error::Unauthorized`]. Fix
//!   the key; no retry helps.
//! * Request: [`Error::InvalidRequest`] (the API's 422, with the body naming
//!   the field), [`Error::InvalidQuestion`] and [`Error::DuplicateQuestionId`]
//!   (caught by the builder before anything is sent) and [`Error::Url`]. Fix
//!   the request; no retry helps either.
//! * Transient, retries exhausted: [`Error::RateLimited`] (429, carrying the
//!   server's `Retry-After` when it sent one) and [`Error::Overloaded`] (529,
//!   which carries no header). Both are returned only after the retry policy
//!   ran out and both carry the attempt count. They are kept apart because
//!   the remedies differ: 429 is the account's rate limit and the remedy is
//!   to slow down; 529 is TypeSafe's capacity and the remedy is to wait.
//! * Transport and decode: [`Error::Transport`] (network, TLS or timeout,
//!   after retries), [`Error::Http`] (any other non-success status, body
//!   truncated) and [`Error::Decode`] (a body that is not the documented
//!   shape). Look at the network, the base URL and the API changelog.
//! * Reading an answer: [`Error::MissingAnswer`],
//!   [`Error::AnswerTypeMismatch`], [`Error::UnknownOption`],
//!   [`Error::NotAProbability`] and [`Error::InvalidAnswer`]. The handle, the
//!   option set or a recording does not match the answer; fix the code or
//!   record again.
//! * Recordings: [`Error::Io`] and [`Error::NoRecording`]. Fix the path, or
//!   record the request before replaying it.
//!
//! Reading and writing recording files by case id, in [`crate::eval`], has
//! its own smaller [`crate::eval::Error`]: a harness handles a missing file
//! differently from a missing answer, and the backends map it into this type
//! where the two meet.

use std::time::Duration;

/// Everything that can go wrong talking to TypeSafe or reading its answers.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// No key was passed to the builder and `TYPESAFE_API_KEY` is unset or
    /// blank. Set one or the other.
    #[error("no API key: set TYPESAFE_API_KEY or pass one to the client builder")]
    MissingApiKey,
    /// The API rejected the key (HTTP 401). Not retried: a retry cannot fix
    /// a key. Check that the key is the right one and still valid.
    #[error("authentication failed (401): check the API key")]
    Unauthorized,
    /// The request body failed server-side validation (HTTP 422). Not
    /// retried: a retry cannot fix a body. `detail` names the offending
    /// field; fix the question or the state it points at.
    #[error("request rejected by the API (422): {detail}")]
    InvalidRequest {
        /// The response body, which names the offending field.
        detail: String,
    },
    /// Rate limited (HTTP 429) and retries were exhausted. The remedy is to
    /// slow down: wait `retry_after` when the server gave one, and lower the
    /// request rate or raise the account's limit if it keeps happening.
    #[error("rate limited (429) after {attempts} attempts{}", crate::error::retry_after_suffix(*.retry_after))]
    RateLimited {
        /// Total attempts made, including the first.
        attempts: u32,
        /// Server-provided `Retry-After`, when present on the last response.
        retry_after: Option<Duration>,
    },
    /// TypeSafe overloaded (HTTP 529) and retries were exhausted. Nothing on
    /// the caller's side is wrong; wait and try again later.
    #[error("service overloaded (529) after {attempts} attempts")]
    Overloaded {
        /// Total attempts made, including the first.
        attempts: u32,
    },
    /// Any other non-success HTTP status: a proxy in the way, a base URL
    /// that is not the API, or a status the API did not have when this crate
    /// was written. The truncated body says which.
    #[error("unexpected HTTP status {status}: {body}")]
    Http {
        /// Status code.
        status: u16,
        /// Response body, truncated.
        body: String,
    },
    /// Network failure, TLS failure or timeout, after retries. Check
    /// connectivity, the base URL and the per-attempt timeout; `attempts`
    /// says how many times it was tried.
    #[cfg(feature = "http")]
    #[error("transport error after {attempts} attempts: {source}")]
    Transport {
        /// Total attempts made, including the first.
        attempts: u32,
        /// Underlying reqwest error.
        #[source]
        source: reqwest::Error,
    },
    /// The response was not the JSON shape the API documents, or a
    /// recording was not one. The API changed, the base URL points at
    /// something else, or the file is corrupt; the message says where.
    #[error("could not decode API response: {0}")]
    Decode(#[from] serde_json::Error),
    /// A recording could not be read or written. Check the directory and its
    /// permissions; `context` names what was being accessed.
    #[error("cannot access {context}: {source}")]
    Io {
        /// What was being accessed.
        context: String,
        /// Cause.
        #[source]
        source: std::io::Error,
    },
    /// A replay had no recording for the request. Run it once through a
    /// [`crate::Recorder`] over the same directory, then replay.
    #[error("no recording for request {0}; record it first")]
    NoRecording(String),
    /// A question id was added twice to one request. Rename one; ids are the
    /// keys answers come back under, so they must be unique.
    #[error("duplicate question id {0:?}")]
    DuplicateQuestionId(String),
    /// A question was built with criteria the API would reject: too few or
    /// too many options or levels. Caught before sending; `reason` says which
    /// limit.
    #[error("invalid question {id:?}: {reason}")]
    InvalidQuestion {
        /// Question id.
        id: String,
        /// What is wrong.
        reason: String,
    },
    /// The response has no answer under the requested id: the handle belongs
    /// to a different request, or a [`crate::Fake`] was built without that
    /// answer.
    #[error("no answer for question {0:?}")]
    MissingAnswer(String),
    /// The answer under this id is a different primitive than the handle
    /// expects: the handle was made for another question with the same id, or
    /// a recording was made with a different question set. Fix the code or
    /// record again.
    #[error("answer {id:?} is a {actual} but a {expected} was requested")]
    AnswerTypeMismatch {
        /// Question id.
        id: String,
        /// Primitive the handle expected.
        expected: &'static str,
        /// Primitive the API returned.
        actual: &'static str,
    },
    /// A Choice answer named an option that is not in the Rust option set:
    /// the enum changed since the answer was recorded, or the backend answered
    /// a different question. Fix the enum or record again.
    #[error("answer {id:?} chose unknown option {option:?}")]
    UnknownOption {
        /// Question id.
        id: String,
        /// The option string the API returned.
        option: String,
    },
    /// The answer is of the right primitive but does not describe the
    /// question it answers: a Score on a different scale, for example. This
    /// crate never produces it; it is for an application's own reading of an
    /// answer, so its errors share one type with the crate's.
    #[error("answer {id:?} does not fit its question: {reason}")]
    InvalidAnswer {
        /// Question id.
        id: String,
        /// What does not fit.
        reason: String,
    },
    /// A probability or confidence was outside `[0, 1]`: the wire sent one,
    /// or a [`crate::Fake`] was built with one. The value is reported.
    #[error("value {value} is not a probability in [0, 1]")]
    NotAProbability {
        /// The offending value.
        value: f64,
    },
    /// The base URL does not parse or cannot be joined with a path. Fix the
    /// URL given to the builder.
    #[error("invalid URL: {0}")]
    Url(String),
}

/// Convenience alias.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// The `Retry-After` clause of a rate-limit error message, empty when the
/// server sent none. Shared with the error types of other clients built on
/// [`crate::http`].
pub fn retry_after_suffix(retry_after: Option<Duration>) -> String {
    retry_after
        .map(|d| format!("; server asked to retry after {}s", d.as_secs_f64()))
        .unwrap_or_default()
}
