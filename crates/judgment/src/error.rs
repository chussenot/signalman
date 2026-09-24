//! Error type for the client and the typed answer layer.

use std::time::Duration;

/// Everything that can go wrong talking to TypeSafe or reading its answers.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// `TYPESAFE_API_KEY` (or an explicit key) was not provided.
    #[error("no API key: set TYPESAFE_API_KEY or pass one to the client builder")]
    MissingApiKey,
    /// The API rejected the key (HTTP 401).
    #[error("authentication failed (401): check the API key")]
    Unauthorized,
    /// The request body failed server-side validation (HTTP 422).
    #[error("request rejected by the API (422): {detail}")]
    InvalidRequest {
        /// The response body, which names the offending field.
        detail: String,
    },
    /// Rate limited (HTTP 429) and retries were exhausted.
    #[error("rate limited (429) after {attempts} attempts{}", crate::error::retry_after_suffix(*.retry_after))]
    RateLimited {
        /// Total attempts made, including the first.
        attempts: u32,
        /// Server-provided `Retry-After`, when present on the last response.
        retry_after: Option<Duration>,
    },
    /// Service overloaded (HTTP 529) and retries were exhausted.
    #[error("service overloaded (529) after {attempts} attempts")]
    Overloaded {
        /// Total attempts made, including the first.
        attempts: u32,
    },
    /// Any other non-success HTTP status.
    #[error("unexpected HTTP status {status}: {body}")]
    Http {
        /// Status code.
        status: u16,
        /// Response body, truncated.
        body: String,
    },
    /// Network failure, TLS failure or timeout, after retries.
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
    /// recording was not one.
    #[error("could not decode API response: {0}")]
    Decode(#[from] serde_json::Error),
    /// A recording could not be read or written.
    #[error("cannot access {context}: {source}")]
    Io {
        /// What was being accessed.
        context: String,
        /// Cause.
        #[source]
        source: std::io::Error,
    },
    /// A replay had no recording for the request.
    #[error("no recording for request {0}; record it first")]
    NoRecording(String),
    /// A question id was added twice to one request.
    #[error("duplicate question id {0:?}")]
    DuplicateQuestionId(String),
    /// A question was built with invalid criteria.
    #[error("invalid question {id:?}: {reason}")]
    InvalidQuestion {
        /// Question id.
        id: String,
        /// What is wrong.
        reason: String,
    },
    /// The response has no answer under the requested id.
    #[error("no answer for question {0:?}")]
    MissingAnswer(String),
    /// The answer under this id is a different primitive than the handle expects.
    #[error("answer {id:?} is a {actual} but a {expected} was requested")]
    AnswerTypeMismatch {
        /// Question id.
        id: String,
        /// Primitive the handle expected.
        expected: &'static str,
        /// Primitive the API returned.
        actual: &'static str,
    },
    /// A Choice answer named an option that is not in the Rust option set.
    #[error("answer {id:?} chose unknown option {option:?}")]
    UnknownOption {
        /// Question id.
        id: String,
        /// The option string the API returned.
        option: String,
    },
    /// The answer is of the right primitive but does not describe the
    /// question it answers: a Score on a different scale, for example.
    #[error("answer {id:?} does not fit its question: {reason}")]
    InvalidAnswer {
        /// Question id.
        id: String,
        /// What does not fit.
        reason: String,
    },
    /// A probability or confidence was outside `[0, 1]`.
    #[error("value {value} is not a probability in [0, 1]")]
    NotAProbability {
        /// The offending value.
        value: f64,
    },
    /// Invalid base URL or path.
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
