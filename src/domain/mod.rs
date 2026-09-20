//! Domain model.
//!
//! Every type in this module upholds an invariant that is checked **once**, at
//! construction, and then carried by the type itself. Handlers, storage and
//! serialisation code never re-validate: if you hold an [`Email`], it is a
//! well-formed email address. This is the "parse, don't validate" principle.

mod money;
mod order;
mod user;

pub use money::Money;
pub use order::{AnyOrder, Draft, Order, OrderId, OrderStatus, Paid, Submitted};
pub use user::{Email, User, UserId, Username};

/// A domain invariant was violated while constructing a value.
///
/// Each variant names the offending field so the API layer can produce a
/// precise 422 response without string-matching on messages.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DomainError {
    /// The email address is syntactically invalid.
    #[error("invalid email address: {0}")]
    InvalidEmail(String),
    /// The username violates the character or length rules.
    #[error("invalid username: {0}")]
    InvalidUsername(String),
    /// A monetary amount is negative or overflowed.
    #[error("invalid amount: {0}")]
    InvalidAmount(String),
    /// A request field could not be parsed into its domain type.
    ///
    /// Produced at the API boundary when JSON deserialisation into a validated
    /// newtype fails; carries serde's message, which includes the field path.
    #[error("invalid field: {0}")]
    InvalidField(String),
}
