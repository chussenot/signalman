//! incident.io client errors, shaped after the API's documented error body.

use std::time::Duration;

use serde::Deserialize;

/// One entry of the API's `errors` array.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ApiErrorDetail {
    /// Machine-readable code such as `is_required`.
    pub code: String,
    /// Human-readable message.
    pub message: String,
    /// Which field, when applicable.
    #[serde(default)]
    pub source: Option<ErrorSource>,
}

/// Field reference inside an error detail.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ErrorSource {
    /// Field name.
    #[serde(default)]
    pub field: Option<String>,
}

/// The documented error body: `type`, `status`, `request_id`, `errors`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Default)]
pub struct ApiErrorBody {
    /// Error class, e.g. `validation_error`, `too_many_requests`.
    #[serde(rename = "type", default)]
    pub kind: String,
    /// Request id to quote to support.
    #[serde(default)]
    pub request_id: String,
    /// Individual errors.
    #[serde(default)]
    pub errors: Vec<ApiErrorDetail>,
}

impl ApiErrorBody {
    fn parse(body: &str) -> Self {
        serde_json::from_str(body).unwrap_or_default()
    }

    fn summary(&self) -> String {
        let msgs: Vec<String> = self
            .errors
            .iter()
            .map(
                |e| match e.source.as_ref().and_then(|s| s.field.as_deref()) {
                    Some(f) => format!("{} ({f}): {}", e.code, e.message),
                    None => format!("{}: {}", e.code, e.message),
                },
            )
            .collect();
        if msgs.is_empty() {
            self.kind.clone()
        } else {
            msgs.join("; ")
        }
    }
}

/// Everything that can go wrong talking to incident.io.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// `INCIDENTIO_API_KEY` (or an explicit key) was not provided.
    #[error("no incident.io API key: set INCIDENTIO_API_KEY or pass one to the client builder")]
    MissingApiKey,
    /// HTTP 401.
    #[error("incident.io rejected the credentials (401){}", request_suffix(.request_id))]
    Unauthorized {
        /// Request id, when the body carried one.
        request_id: String,
    },
    /// HTTP 403: the key lacks a scope.
    #[error("incident.io forbade the request (403): {detail}{}", request_suffix(.request_id))]
    Forbidden {
        /// Error summary.
        detail: String,
        /// Request id.
        request_id: String,
    },
    /// HTTP 404.
    #[error("incident.io resource not found (404){}", request_suffix(.request_id))]
    NotFound {
        /// Request id.
        request_id: String,
    },
    /// HTTP 422 with field-level detail.
    #[error("incident.io rejected the request (422): {detail}{}", request_suffix(.request_id))]
    Validation {
        /// Error summary including field names.
        detail: String,
        /// Request id.
        request_id: String,
    },
    /// HTTP 429 after retries.
    #[error("incident.io rate limited the request (429) after {attempts} attempts{}", crate::http::retry_after_suffix(*.retry_after))]
    RateLimited {
        /// Attempts made, including the first.
        attempts: u32,
        /// The last response's wait (`retry-after-ms`, or `Retry-After` in
        /// seconds or as an HTTP date), when it named one.
        retry_after: Option<Duration>,
    },
    /// Any other non-success status.
    #[error("incident.io returned HTTP {status}: {body}")]
    Http {
        /// Status code.
        status: u16,
        /// Truncated body.
        body: String,
    },
    /// Network, TLS or timeout, after retries.
    #[error("transport error talking to incident.io after {attempts} attempts: {source}")]
    Transport {
        /// Attempts made, including the first.
        attempts: u32,
        /// Underlying error.
        #[source]
        source: reqwest::Error,
    },
    /// Response JSON did not match the expected shape.
    #[error("could not decode incident.io response: {0}")]
    Decode(#[from] serde_json::Error),
    /// Bad base URL or path.
    #[error("invalid URL: {0}")]
    Url(String),
}

impl Error {
    /// Classify a completed non-success response.
    pub(crate) fn from_response(
        status: u16,
        body: String,
        attempts: u32,
        retry_after: Option<Duration>,
    ) -> Self {
        let parsed = ApiErrorBody::parse(&body);
        match status {
            401 => Self::Unauthorized {
                request_id: parsed.request_id,
            },
            403 => Self::Forbidden {
                detail: parsed.summary(),
                request_id: parsed.request_id,
            },
            404 => Self::NotFound {
                request_id: parsed.request_id,
            },
            422 => Self::Validation {
                detail: parsed.summary(),
                request_id: parsed.request_id,
            },
            429 => Self::RateLimited {
                attempts,
                retry_after,
            },
            _ => Self::Http {
                status,
                body: crate::http::truncate(body),
            },
        }
    }
}

fn request_suffix(request_id: &str) -> String {
    if request_id.is_empty() {
        String::new()
    } else {
        format!(" [request_id {request_id}]")
    }
}

/// Convenience alias.
pub type Result<T, E = Error> = std::result::Result<T, E>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validation_error_summarises_fields() {
        let body = r#"{"type":"validation_error","status":422,"request_id":"req-1",
            "errors":[{"code":"is_required","message":"A severity is required","source":{"field":"severity_id"}}]}"#;
        let err = Error::from_response(422, body.to_owned(), 1, None);
        let text = err.to_string();
        assert!(text.contains("is_required (severity_id)"), "{text}");
        assert!(text.contains("request_id req-1"), "{text}");
    }

    #[test]
    fn unparseable_body_still_classifies() {
        assert!(matches!(
            Error::from_response(404, "<html>".into(), 1, None),
            Error::NotFound { .. }
        ));
    }
}
