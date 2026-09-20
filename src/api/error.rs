//! API error type.
//!
//! One enum, one `IntoResponse`. Handlers return `Result<_, ApiError>` and use
//! `?`; the mapping from domain and store errors to HTTP status codes lives
//! here and nowhere else. Bodies follow RFC 9457 *Problem Details*.

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use utoipa::ToSchema;

use crate::domain::{DomainError, OrderStatus};
use crate::store::{StoreError, UpdateError};

/// All failures an endpoint can surface.
#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    /// The body was not valid JSON, or the content type was wrong.
    #[error("malformed request: {0}")]
    BadRequest(String),
    /// The body was valid JSON but violated a domain invariant.
    #[error(transparent)]
    Validation(#[from] DomainError),
    /// The requested resource does not exist.
    #[error("{0}")]
    NotFound(String),
    /// The request conflicts with current state (uniqueness, lifecycle).
    #[error("{0}")]
    Conflict(String),
    /// The order is not in a state that allows the requested transition.
    #[error("order is {actual}, expected {expected}")]
    InvalidTransition {
        /// State the order is actually in.
        actual: OrderStatus,
        /// State the operation required.
        expected: OrderStatus,
    },
}

impl ApiError {
    fn status(&self) -> StatusCode {
        match self {
            Self::BadRequest(_) => StatusCode::BAD_REQUEST,
            Self::Validation(_) => StatusCode::UNPROCESSABLE_ENTITY,
            Self::NotFound(_) => StatusCode::NOT_FOUND,
            Self::Conflict(_) | Self::InvalidTransition { .. } => StatusCode::CONFLICT,
        }
    }
}

impl From<StoreError> for ApiError {
    fn from(err: StoreError) -> Self {
        match err {
            StoreError::UsernameTaken(_) | StoreError::EmailTaken(_) => {
                Self::Conflict(err.to_string())
            }
            StoreError::UserNotFound(_) | StoreError::OrderNotFound(_) => {
                Self::NotFound(err.to_string())
            }
        }
    }
}

impl From<UpdateError<ApiError>> for ApiError {
    fn from(err: UpdateError<ApiError>) -> Self {
        match err {
            UpdateError::Store(e) => e.into(),
            UpdateError::Transition(e) => e,
        }
    }
}

impl From<JsonRejection> for ApiError {
    fn from(rejection: JsonRejection) -> Self {
        match rejection {
            // Valid JSON that failed to deserialise into the target type:
            // this is where `serde(try_from)` newtype errors land.
            JsonRejection::JsonDataError(e) => {
                let text = e.body_text();
                let detail = text
                    .strip_prefix("Failed to deserialize the JSON body into the target type: ")
                    .unwrap_or(&text);
                Self::Validation(DomainError::InvalidField(detail.to_owned()))
            }
            other => Self::BadRequest(other.body_text()),
        }
    }
}

/// RFC 9457 problem details body.
#[derive(Debug, Serialize, ToSchema)]
pub struct Problem {
    /// Short, human-readable summary of the problem type.
    #[schema(example = "Unprocessable Entity")]
    pub title: String,
    /// HTTP status code.
    #[schema(example = 422)]
    pub status: u16,
    /// Human-readable explanation specific to this occurrence.
    #[schema(example = "invalid email address: missing '@'")]
    pub detail: String,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = self.status();
        let problem = Problem {
            title: status.canonical_reason().unwrap_or("Error").to_owned(),
            status: status.as_u16(),
            detail: self.to_string(),
        };
        if status.is_server_error() {
            tracing::error!(error = %self, "request failed");
        } else {
            tracing::debug!(error = %self, status = status.as_u16(), "request rejected");
        }
        (
            status,
            [(header::CONTENT_TYPE, "application/problem+json")],
            Json(problem),
        )
            .into_response()
    }
}
