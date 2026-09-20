//! `/orders` endpoints.
//!
//! Each transition handler matches the stored [`AnyOrder`] exactly once. The
//! arm that matches hands back a concrete `Order<S>`, on which only the legal
//! transition method exists. Every other arm is a 409 with both the actual
//! and the expected state.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::error::{ApiError, Problem};
use super::extract::ValidJson;
use crate::domain::{AnyOrder, Money, Order, OrderId, OrderStatus, UserId};
use crate::store::Store;

/// Request body for [`create_order`].
#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateOrderRequest {
    /// Owning customer. Must exist.
    pub customer_id: UserId,
    /// Initial total in minor units. Must be non-negative.
    pub total: Money,
}

/// Request body for [`add_to_order`].
#[derive(Debug, Deserialize, ToSchema)]
pub struct AddAmountRequest {
    /// Amount to add, in minor units.
    pub amount: Money,
}

/// API representation of an order in any state.
#[derive(Debug, Serialize, ToSchema)]
pub struct OrderResponse {
    /// Stable identifier.
    pub id: OrderId,
    /// Owning customer.
    pub customer_id: UserId,
    /// Total in minor units.
    pub total: Money,
    /// Lifecycle state.
    pub status: OrderStatus,
    /// Present once submitted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub submitted_at: Option<DateTime<Utc>>,
    /// Present once paid.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub paid_at: Option<DateTime<Utc>>,
}

impl From<AnyOrder> for OrderResponse {
    fn from(order: AnyOrder) -> Self {
        Self {
            id: order.id(),
            customer_id: order.customer(),
            total: order.total(),
            status: order.status(),
            submitted_at: order.submitted_at(),
            paid_at: order.paid_at(),
        }
    }
}

/// Open a draft order.
#[utoipa::path(
    post,
    path = "/orders",
    tag = "orders",
    request_body = CreateOrderRequest,
    responses(
        (status = 201, description = "Draft order created", body = OrderResponse),
        (status = 404, description = "Customer does not exist", body = Problem),
        (status = 422, description = "Invalid amount", body = Problem),
    )
)]
pub async fn create_order(
    State(store): State<Store>,
    ValidJson(body): ValidJson<CreateOrderRequest>,
) -> Result<(StatusCode, Json<OrderResponse>), ApiError> {
    let draft = Order::draft(body.customer_id, body.total);
    let stored = store.put_order(draft.into()).await?;
    tracing::info!(order_id = %stored.id(), "order drafted");
    Ok((StatusCode::CREATED, Json(stored.into())))
}

/// Fetch an order by id.
#[utoipa::path(
    get,
    path = "/orders/{id}",
    tag = "orders",
    params(("id" = OrderId, Path, description = "Order identifier")),
    responses(
        (status = 200, description = "The order", body = OrderResponse),
        (status = 404, description = "No such order", body = Problem),
    )
)]
pub async fn get_order(
    State(store): State<Store>,
    Path(id): Path<OrderId>,
) -> Result<Json<OrderResponse>, ApiError> {
    store
        .get_order(id)
        .await
        .map(|o| Json(o.into()))
        .ok_or_else(|| ApiError::NotFound(format!("order {id} not found")))
}

/// Add an amount to a draft order.
#[utoipa::path(
    post,
    path = "/orders/{id}/items",
    tag = "orders",
    params(("id" = OrderId, Path, description = "Order identifier")),
    request_body = AddAmountRequest,
    responses(
        (status = 200, description = "Updated draft", body = OrderResponse),
        (status = 404, description = "No such order", body = Problem),
        (status = 409, description = "Order is no longer a draft", body = Problem),
        (status = 422, description = "Amount invalid or total overflowed", body = Problem),
    )
)]
pub async fn add_to_order(
    State(store): State<Store>,
    Path(id): Path<OrderId>,
    ValidJson(body): ValidJson<AddAmountRequest>,
) -> Result<Json<OrderResponse>, ApiError> {
    let updated = store
        .update_order(id, |order| match order {
            AnyOrder::Draft(draft) => Ok(draft.add_amount(body.amount)?.into()),
            other => Err(invalid_transition(&other, OrderStatus::Draft)),
        })
        .await?;
    Ok(Json(updated.into()))
}

/// Submit a draft order for payment.
#[utoipa::path(
    post,
    path = "/orders/{id}/submit",
    tag = "orders",
    params(("id" = OrderId, Path, description = "Order identifier")),
    responses(
        (status = 200, description = "Order submitted", body = OrderResponse),
        (status = 404, description = "No such order", body = Problem),
        (status = 409, description = "Order is not a draft", body = Problem),
        (status = 422, description = "Order total is zero", body = Problem),
    )
)]
pub async fn submit_order(
    State(store): State<Store>,
    Path(id): Path<OrderId>,
) -> Result<Json<OrderResponse>, ApiError> {
    let updated = store
        .update_order(id, |order| match order {
            AnyOrder::Draft(draft) => Ok(draft.submit()?.into()),
            other => Err(invalid_transition(&other, OrderStatus::Draft)),
        })
        .await?;
    tracing::info!(order_id = %id, "order submitted");
    Ok(Json(updated.into()))
}

/// Record payment for a submitted order.
#[utoipa::path(
    post,
    path = "/orders/{id}/pay",
    tag = "orders",
    params(("id" = OrderId, Path, description = "Order identifier")),
    responses(
        (status = 200, description = "Order paid", body = OrderResponse),
        (status = 404, description = "No such order", body = Problem),
        (status = 409, description = "Order has not been submitted", body = Problem),
    )
)]
pub async fn pay_order(
    State(store): State<Store>,
    Path(id): Path<OrderId>,
) -> Result<Json<OrderResponse>, ApiError> {
    let updated = store
        .update_order(id, |order| match order {
            AnyOrder::Submitted(submitted) => Ok(submitted.pay().into()),
            other => Err(invalid_transition(&other, OrderStatus::Submitted)),
        })
        .await?;
    tracing::info!(order_id = %id, "order paid");
    Ok(Json(updated.into()))
}

fn invalid_transition(order: &AnyOrder, expected: OrderStatus) -> ApiError {
    ApiError::InvalidTransition {
        actual: order.status(),
        expected,
    }
}
