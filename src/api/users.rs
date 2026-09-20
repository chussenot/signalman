//! `/users` endpoints.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::Deserialize;
use utoipa::ToSchema;

use super::error::{ApiError, Problem};
use super::extract::ValidJson;
use crate::domain::{Email, User, UserId, Username};
use crate::store::Store;

/// Request body for [`create_user`].
///
/// The fields are the domain newtypes themselves, so deserialisation *is*
/// validation. There is no separate DTO to keep in sync.
#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateUserRequest {
    /// Desired handle.
    pub username: Username,
    /// Contact address.
    pub email: Email,
}

/// Register a new user.
#[utoipa::path(
    post,
    path = "/users",
    tag = "users",
    request_body = CreateUserRequest,
    responses(
        (status = 201, description = "User created", body = User),
        (status = 400, description = "Malformed JSON", body = Problem),
        (status = 409, description = "Username or email already taken", body = Problem),
        (status = 422, description = "A field violates a domain invariant", body = Problem),
    )
)]
pub async fn create_user(
    State(store): State<Store>,
    ValidJson(body): ValidJson<CreateUserRequest>,
) -> Result<(StatusCode, Json<User>), ApiError> {
    let user = store
        .insert_user(User::new(body.username, body.email))
        .await?;
    tracing::info!(user_id = %user.id, "user created");
    Ok((StatusCode::CREATED, Json(user)))
}

/// Fetch a user by id.
#[utoipa::path(
    get,
    path = "/users/{id}",
    tag = "users",
    params(("id" = UserId, Path, description = "User identifier")),
    responses(
        (status = 200, description = "The user", body = User),
        (status = 404, description = "No such user", body = Problem),
    )
)]
pub async fn get_user(
    State(store): State<Store>,
    Path(id): Path<UserId>,
) -> Result<Json<User>, ApiError> {
    store
        .get_user(id)
        .await
        .map(Json)
        .ok_or_else(|| ApiError::NotFound(format!("user {id} not found")))
}

/// List all users.
#[utoipa::path(
    get,
    path = "/users",
    tag = "users",
    responses((status = 200, description = "All users", body = Vec<User>))
)]
pub async fn list_users(State(store): State<Store>) -> Json<Vec<User>> {
    Json(store.list_users().await)
}
