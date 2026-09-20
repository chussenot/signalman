//! Order lifecycle modelled with the **typestate** pattern.
//!
//! `Order<Draft>`, `Order<Submitted>` and `Order<Paid>` are distinct types.
//! Each transition method is implemented only for the state that permits it,
//! so `order.pay()` on a draft is a compile-time error, not a runtime check:
//!
//! ```compile_fail
//! use rustsafe::domain::{Money, Order, UserId};
//! let draft = Order::draft(UserId::new(), Money::ZERO);
//! let _ = draft.pay(); // error[E0599]: no method named `pay` found for `Order<Draft>`
//! ```
//!
//! The valid path compiles:
//!
//! ```
//! use rustsafe::domain::{Money, Order, UserId};
//! let draft = Order::draft(UserId::new(), Money::from_minor(1000).unwrap());
//! let submitted = draft.submit().unwrap();
//! let paid = submitted.pay();
//! assert_eq!(paid.total().minor(), 1000);
//! ```
//!
//! Persisted orders arrive with their state known only at runtime, so the
//! boundary uses [`AnyOrder`], a closed enum over the three concrete types.
//! Matching on it is the single place where "which state am I in" is decided;
//! everything after that is statically typed again.

use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use super::{DomainError, Money, UserId};

/// Opaque identifier for an [`Order`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(transparent)]
#[schema(value_type = String, format = Uuid)]
pub struct OrderId(Uuid);

impl OrderId {
    /// Generate a fresh random identifier.
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for OrderId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for OrderId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// Marker trait for order states. Sealed: only the three states below exist.
pub trait OrderState: private::Sealed + fmt::Debug + Clone + Send + Sync + 'static {
    /// The status label exposed over the API.
    const STATUS: OrderStatus;
}

mod private {
    pub trait Sealed {}
    impl Sealed for super::Draft {}
    impl Sealed for super::Submitted {}
    impl Sealed for super::Paid {}
}

/// The order is being assembled and may still change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Draft;

/// The order has been submitted and is awaiting payment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Submitted {
    /// When the customer submitted the order.
    pub submitted_at: DateTime<Utc>,
}

/// The order has been paid; it is now immutable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paid {
    /// When the customer submitted the order.
    pub submitted_at: DateTime<Utc>,
    /// When payment was recorded.
    pub paid_at: DateTime<Utc>,
}

impl OrderState for Draft {
    const STATUS: OrderStatus = OrderStatus::Draft;
}
impl OrderState for Submitted {
    const STATUS: OrderStatus = OrderStatus::Submitted;
}
impl OrderState for Paid {
    const STATUS: OrderStatus = OrderStatus::Paid;
}

/// Runtime label for an order's lifecycle state, used in API responses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum OrderStatus {
    /// See [`Draft`].
    Draft,
    /// See [`Submitted`].
    Submitted,
    /// See [`Paid`].
    Paid,
}

impl fmt::Display for OrderStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Draft => "draft",
            Self::Submitted => "submitted",
            Self::Paid => "paid",
        };
        f.write_str(s)
    }
}

/// An order in state `S`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Order<S: OrderState> {
    id: OrderId,
    customer: UserId,
    total: Money,
    state: S,
}

// Accessors available in every state.
impl<S: OrderState> Order<S> {
    /// Stable identifier.
    pub fn id(&self) -> OrderId {
        self.id
    }

    /// The customer who owns this order.
    pub fn customer(&self) -> UserId {
        self.customer
    }

    /// Current total.
    pub fn total(&self) -> Money {
        self.total
    }

    /// Runtime status label.
    pub fn status(&self) -> OrderStatus {
        S::STATUS
    }

    /// State-specific data.
    pub fn state(&self) -> &S {
        &self.state
    }
}

impl Order<Draft> {
    /// Open a new draft order for a customer.
    pub fn draft(customer: UserId, total: Money) -> Self {
        Self {
            id: OrderId::new(),
            customer,
            total,
            state: Draft,
        }
    }

    /// Add an amount to the draft total. Only drafts can change.
    pub fn add_amount(mut self, amount: Money) -> Result<Self, DomainError> {
        self.total = self.total.checked_add(amount)?;
        Ok(self)
    }

    /// Submit the draft. Consumes the draft so it cannot be edited afterwards.
    ///
    /// # Errors
    /// An empty order cannot be submitted.
    pub fn submit(self) -> Result<Order<Submitted>, DomainError> {
        if self.total.is_zero() {
            return Err(DomainError::InvalidAmount(
                "cannot submit an order with a zero total".into(),
            ));
        }
        Ok(Order {
            id: self.id,
            customer: self.customer,
            total: self.total,
            state: Submitted {
                submitted_at: Utc::now(),
            },
        })
    }
}

impl Order<Submitted> {
    /// Record payment. Infallible: a submitted order is always payable.
    pub fn pay(self) -> Order<Paid> {
        Order {
            id: self.id,
            customer: self.customer,
            total: self.total,
            state: Paid {
                submitted_at: self.state.submitted_at,
                paid_at: Utc::now(),
            },
        }
    }
}

/// An order whose state is only known at runtime (e.g. loaded from storage).
///
/// This is the *only* place the state is dynamic. Once matched, the inner
/// value is a fully typed `Order<S>` again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnyOrder {
    /// See [`Draft`].
    Draft(Order<Draft>),
    /// See [`Submitted`].
    Submitted(Order<Submitted>),
    /// See [`Paid`].
    Paid(Order<Paid>),
}

impl AnyOrder {
    /// Stable identifier, regardless of state.
    pub fn id(&self) -> OrderId {
        match self {
            Self::Draft(o) => o.id(),
            Self::Submitted(o) => o.id(),
            Self::Paid(o) => o.id(),
        }
    }

    /// Owning customer, regardless of state.
    pub fn customer(&self) -> UserId {
        match self {
            Self::Draft(o) => o.customer(),
            Self::Submitted(o) => o.customer(),
            Self::Paid(o) => o.customer(),
        }
    }

    /// Current total, regardless of state.
    pub fn total(&self) -> Money {
        match self {
            Self::Draft(o) => o.total(),
            Self::Submitted(o) => o.total(),
            Self::Paid(o) => o.total(),
        }
    }

    /// Runtime status label.
    pub fn status(&self) -> OrderStatus {
        match self {
            Self::Draft(o) => o.status(),
            Self::Submitted(o) => o.status(),
            Self::Paid(o) => o.status(),
        }
    }

    /// Submission timestamp, present once submitted.
    pub fn submitted_at(&self) -> Option<DateTime<Utc>> {
        match self {
            Self::Draft(_) => None,
            Self::Submitted(o) => Some(o.state().submitted_at),
            Self::Paid(o) => Some(o.state().submitted_at),
        }
    }

    /// Payment timestamp, present once paid.
    pub fn paid_at(&self) -> Option<DateTime<Utc>> {
        match self {
            Self::Draft(_) | Self::Submitted(_) => None,
            Self::Paid(o) => Some(o.state().paid_at),
        }
    }
}

impl From<Order<Draft>> for AnyOrder {
    fn from(o: Order<Draft>) -> Self {
        Self::Draft(o)
    }
}
impl From<Order<Submitted>> for AnyOrder {
    fn from(o: Order<Submitted>) -> Self {
        Self::Submitted(o)
    }
}
impl From<Order<Paid>> for AnyOrder {
    fn from(o: Order<Paid>) -> Self {
        Self::Paid(o)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn money(minor: i64) -> Money {
        Money::from_minor(minor).unwrap()
    }

    #[test]
    fn happy_path_transitions_carry_data_forward() {
        let draft = Order::draft(UserId::new(), money(500))
            .add_amount(money(250))
            .unwrap();
        let id = draft.id();
        let submitted = draft.submit().unwrap();
        assert_eq!(submitted.id(), id);
        assert_eq!(submitted.status(), OrderStatus::Submitted);
        let paid = submitted.pay();
        assert_eq!(paid.total(), money(750));
        assert!(paid.state().submitted_at <= paid.state().paid_at);
    }

    #[test]
    fn empty_draft_cannot_be_submitted() {
        let err = Order::draft(UserId::new(), Money::ZERO)
            .submit()
            .unwrap_err();
        assert!(matches!(err, DomainError::InvalidAmount(_)));
    }

    #[test]
    fn any_order_exposes_state_dependent_timestamps() {
        let draft: AnyOrder = Order::draft(UserId::new(), money(1)).into();
        assert_eq!(draft.submitted_at(), None);
        assert_eq!(draft.paid_at(), None);

        let AnyOrder::Draft(d) = draft else {
            unreachable!()
        };
        let paid: AnyOrder = d.submit().unwrap().pay().into();
        assert!(paid.submitted_at().is_some());
        assert!(paid.paid_at().is_some());
        assert_eq!(paid.status(), OrderStatus::Paid);
    }
}
