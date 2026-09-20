//! In-memory storage.
//!
//! Maps are keyed by the strongly typed identifiers, so a `UserId` cannot be
//! used to look up an order by accident. Swap this for a database-backed
//! implementation without touching the domain or API layers.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::RwLock;

use crate::domain::{AnyOrder, Email, OrderId, User, UserId, Username};

/// Shared, thread-safe application store.
#[derive(Debug, Clone, Default)]
pub struct Store {
    inner: Arc<RwLock<Inner>>,
}

#[derive(Debug, Default)]
struct Inner {
    users: HashMap<UserId, User>,
    orders: HashMap<OrderId, AnyOrder>,
}

/// Why a write to the store was rejected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StoreError {
    /// Another user already holds this username.
    #[error("username '{0}' is already taken")]
    UsernameTaken(Username),
    /// Another user already holds this email.
    #[error("email '{0}' is already registered")]
    EmailTaken(Email),
    /// The referenced user does not exist.
    #[error("user {0} not found")]
    UserNotFound(UserId),
    /// The referenced order does not exist.
    #[error("order {0} not found")]
    OrderNotFound(OrderId),
}

impl Store {
    /// Create an empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert a user, enforcing uniqueness of username and email.
    pub async fn insert_user(&self, user: User) -> Result<User, StoreError> {
        let mut inner = self.inner.write().await;
        if inner.users.values().any(|u| u.username == user.username) {
            return Err(StoreError::UsernameTaken(user.username));
        }
        if inner.users.values().any(|u| u.email == user.email) {
            return Err(StoreError::EmailTaken(user.email));
        }
        inner.users.insert(user.id, user.clone());
        Ok(user)
    }

    /// Look up a user by id.
    pub async fn get_user(&self, id: UserId) -> Option<User> {
        self.inner.read().await.users.get(&id).cloned()
    }

    /// All users, in unspecified order.
    pub async fn list_users(&self) -> Vec<User> {
        self.inner.read().await.users.values().cloned().collect()
    }

    /// Insert or replace an order. The customer must exist.
    pub async fn put_order(&self, order: AnyOrder) -> Result<AnyOrder, StoreError> {
        let mut inner = self.inner.write().await;
        if !inner.users.contains_key(&order.customer()) {
            return Err(StoreError::UserNotFound(order.customer()));
        }
        inner.orders.insert(order.id(), order.clone());
        Ok(order)
    }

    /// Look up an order by id.
    pub async fn get_order(&self, id: OrderId) -> Option<AnyOrder> {
        self.inner.read().await.orders.get(&id).cloned()
    }

    /// Atomically transform an order in place.
    ///
    /// The closure receives the current [`AnyOrder`] and returns the new one
    /// or an error. Holding the write lock across the closure makes the
    /// read-modify-write a single step, so concurrent transitions on the same
    /// order cannot interleave.
    pub async fn update_order<E>(
        &self,
        id: OrderId,
        f: impl FnOnce(AnyOrder) -> Result<AnyOrder, E>,
    ) -> Result<AnyOrder, UpdateError<E>> {
        let mut inner = self.inner.write().await;
        let current = inner
            .orders
            .get(&id)
            .cloned()
            .ok_or(UpdateError::Store(StoreError::OrderNotFound(id)))?;
        let next = f(current).map_err(UpdateError::Transition)?;
        inner.orders.insert(id, next.clone());
        Ok(next)
    }
}

/// Failure of [`Store::update_order`]: either the lookup or the transition.
#[derive(Debug)]
pub enum UpdateError<E> {
    /// The order could not be found.
    Store(StoreError),
    /// The caller's transition closure rejected the change.
    Transition(E),
}
