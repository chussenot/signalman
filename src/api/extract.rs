//! Request extractors.

use axum::Json;
use axum::extract::{FromRequest, Request};
use serde::de::DeserializeOwned;

use super::error::ApiError;

/// A JSON body deserialised into `T`, with rejections mapped to [`ApiError`].
///
/// Because domain newtypes implement `Deserialize` via `try_from`, a `T` built
/// from validated types is guaranteed valid once this extractor succeeds.
/// Malformed JSON yields 400; well-formed JSON violating an invariant yields
/// 422 with the domain error message.
#[derive(Debug, Clone, Copy)]
pub struct ValidJson<T>(pub T);

impl<S, T> FromRequest<S> for ValidJson<T>
where
    S: Send + Sync,
    T: DeserializeOwned,
{
    type Rejection = ApiError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        let Json(value) = Json::<T>::from_request(req, state).await?;
        Ok(Self(value))
    }
}
