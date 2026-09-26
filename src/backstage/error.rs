//! Backstage client errors.

use std::time::Duration;

/// Everything that can go wrong talking to Backstage.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// `BACKSTAGE_BASE_URL` was not provided.
    #[error("Backstage is not configured: set BACKSTAGE_BASE_URL (and BACKSTAGE_TOKEN)")]
    MissingConfig,
    /// HTTP 401 or 403.
    #[error(
        "Backstage rejected the credentials ({status}); check BACKSTAGE_TOKEN and backend.auth.externalAccess"
    )]
    Unauthorized {
        /// 401 or 403.
        status: u16,
    },
    /// HTTP 404.
    #[error("Backstage resource not found: {path}")]
    NotFound {
        /// Request path.
        path: String,
    },
    /// HTTP 429 after retries.
    #[error("Backstage rate limited the request after {attempts} attempts")]
    RateLimited {
        /// Attempts made.
        attempts: u32,
        /// The last response's wait (`retry-after-ms`, or `Retry-After` in
        /// seconds or as an HTTP date), when it named one.
        retry_after: Option<Duration>,
    },
    /// Any other non-success status.
    #[error("Backstage returned HTTP {status}: {body}")]
    Http {
        /// Status code.
        status: u16,
        /// Truncated body.
        body: String,
    },
    /// A body over the retry policy's `max_body_bytes`, dropped unread and
    /// not retried: Backstage answered with far more than it documents, or
    /// something else answered in its place.
    #[error("Backstage response body over {limit} bytes; not read")]
    ResponseTooLarge {
        /// The cap that was passed, in bytes.
        limit: usize,
    },
    /// Network, TLS or timeout, after retries.
    #[error("transport error talking to Backstage after {attempts} attempts: {source}")]
    Transport {
        /// Attempts made.
        attempts: u32,
        /// Underlying error.
        #[source]
        source: reqwest::Error,
    },
    /// Response JSON did not match the expected shape.
    #[error("could not decode Backstage response: {0}")]
    Decode(#[from] serde_json::Error),
    /// Bad base URL or path.
    #[error("invalid URL: {0}")]
    Url(String),
}

/// Convenience alias.
pub type Result<T, E = Error> = std::result::Result<T, E>;
