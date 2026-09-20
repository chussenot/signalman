//! # rustsafe
//!
//! A small HTTP API that pushes correctness into the type system:
//!
//! * [`domain`] — *parse, don't validate*: newtypes whose only constructors
//!   validate, and a **typestate** order lifecycle where illegal transitions
//!   do not compile.
//! * [`api`] — axum handlers whose request bodies *are* the domain types, one
//!   error enum mapped to RFC 9457 problem details, and an `OpenAPI` document
//!   generated from the same types so the contract cannot drift.
//! * [`store`] — in-memory storage keyed by the strongly typed identifiers.

pub mod api;
pub mod domain;
pub mod store;

pub use api::router;
pub use store::Store;
