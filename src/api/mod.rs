//! HTTP layer: routing, extraction, error mapping and the `OpenAPI` document.

pub mod error;
pub mod extract;
pub mod orders;
pub mod users;

use axum::Router;
use axum::routing::get;
use utoipa::OpenApi;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::store::Store;

/// Root `OpenAPI` document. Paths are attached by [`router`] via `routes!`, so a
/// handler cannot be routed without also being documented.
#[derive(OpenApi)]
#[openapi(
    info(
        title = "rustsafe",
        description = "Type-safe HTTP API: validated newtypes, typestate order lifecycle, schema derived from the types."
    ),
    tags(
        (name = "users", description = "User registration and lookup"),
        (name = "orders", description = "Order lifecycle: draft → submitted → paid")
    )
)]
pub struct ApiDoc;

/// Build the application router with the given store.
///
/// The `OpenAPI` document is served at `/openapi.json` and a liveness probe at
/// `/healthz`.
pub fn router(store: Store) -> Router {
    let (router, api) = OpenApiRouter::with_openapi(ApiDoc::openapi())
        .routes(routes!(users::create_user, users::list_users))
        .routes(routes!(users::get_user))
        .routes(routes!(orders::create_order))
        .routes(routes!(orders::get_order))
        .routes(routes!(orders::add_to_order))
        .routes(routes!(orders::submit_order))
        .routes(routes!(orders::pay_order))
        .with_state(store)
        .split_for_parts();

    router
        .route("/healthz", get(|| async { "ok" }))
        .route("/openapi.json", get(move || async move { axum::Json(api) }))
}
