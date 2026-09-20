use std::borrow::Cow;
use std::fmt;

use serde::{Deserialize, Serialize};
use utoipa::openapi::RefOr;
use utoipa::openapi::schema::{ObjectBuilder, Schema, SchemaType, Type};
use utoipa::{PartialSchema, ToSchema};
use uuid::Uuid;

use super::DomainError;

/// Opaque identifier for a [`User`].
///
/// A distinct type from [`super::OrderId`] even though both wrap a UUID, so the
/// compiler rejects passing one where the other is expected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(transparent)]
#[schema(value_type = String, format = Uuid)]
pub struct UserId(Uuid);

impl UserId {
    /// Generate a fresh random identifier.
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for UserId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for UserId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// A syntactically valid, lower-cased email address.
///
/// Deserialisation goes through [`Email::parse`], so a request body containing
/// an invalid address is rejected before any handler code runs.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(try_from = "String", into = "String")]
#[schema(value_type = String, format = Email, example = "ada@example.org")]
pub struct Email(String);

impl Email {
    const MAX_LEN: usize = 254;

    /// Validate and normalise an email address.
    ///
    /// The check is deliberately conservative (RFC 5322 is not fully
    /// enforced): one `@`, non-empty local part, a domain containing a dot,
    /// no whitespace, bounded length.
    pub fn parse(raw: impl Into<String>) -> Result<Self, DomainError> {
        let raw: String = raw.into();
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(DomainError::InvalidEmail("must not be empty".into()));
        }
        if trimmed.len() > Self::MAX_LEN {
            return Err(DomainError::InvalidEmail(format!(
                "must be at most {} characters",
                Self::MAX_LEN
            )));
        }
        if trimmed.chars().any(char::is_whitespace) {
            return Err(DomainError::InvalidEmail(
                "must not contain whitespace".into(),
            ));
        }
        let Some((local, domain)) = trimmed.split_once('@') else {
            return Err(DomainError::InvalidEmail("missing '@'".into()));
        };
        if local.is_empty() || domain.is_empty() || domain.contains('@') {
            return Err(DomainError::InvalidEmail(
                "must have exactly one '@' with text on both sides".into(),
            ));
        }
        if !domain.contains('.') || domain.starts_with('.') || domain.ends_with('.') {
            return Err(DomainError::InvalidEmail(
                "domain must contain a dot and not start or end with one".into(),
            ));
        }
        Ok(Self(trimmed.to_ascii_lowercase()))
    }

    /// Borrow the normalised address.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for Email {
    type Error = DomainError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(value)
    }
}

impl From<Email> for String {
    fn from(value: Email) -> Self {
        value.0
    }
}

impl fmt::Display for Email {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A username: 3 to 32 characters from `[a-z0-9_]`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Username(String);

// Hand-written schema: the derive macro cannot express length bounds on a
// newtype, and the contract should match `Username::parse` exactly.
impl PartialSchema for Username {
    fn schema() -> RefOr<Schema> {
        ObjectBuilder::new()
            .schema_type(SchemaType::Type(Type::String))
            .min_length(Some(Self::MIN_LEN))
            .max_length(Some(Self::MAX_LEN))
            .pattern(Some("^[a-z0-9_]+$"))
            .description(Some("3 to 32 characters from [a-z0-9_]"))
            .examples([serde_json::json!("ada_lovelace")])
            .into()
    }
}

impl ToSchema for Username {
    fn name() -> Cow<'static, str> {
        Cow::Borrowed("Username")
    }
}

impl Username {
    const MIN_LEN: usize = 3;
    const MAX_LEN: usize = 32;

    /// Validate a username.
    pub fn parse(raw: impl Into<String>) -> Result<Self, DomainError> {
        let raw: String = raw.into();
        let len = raw.chars().count();
        if !(Self::MIN_LEN..=Self::MAX_LEN).contains(&len) {
            return Err(DomainError::InvalidUsername(format!(
                "must be between {} and {} characters",
                Self::MIN_LEN,
                Self::MAX_LEN
            )));
        }
        if !raw
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        {
            return Err(DomainError::InvalidUsername(
                "may only contain lowercase letters, digits and underscores".into(),
            ));
        }
        Ok(Self(raw))
    }

    /// Borrow the username.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for Username {
    type Error = DomainError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(value)
    }
}

impl From<Username> for String {
    fn from(value: Username) -> Self {
        value.0
    }
}

impl fmt::Display for Username {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A registered user.
///
/// Because every field is a validated newtype, a `User` cannot exist in an
/// invalid state. There is no `validate()` method to forget to call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct User {
    /// Stable identifier.
    pub id: UserId,
    /// Unique handle.
    pub username: Username,
    /// Contact address.
    pub email: Email,
}

impl User {
    /// Create a new user with a fresh identifier.
    pub fn new(username: Username, email: Email) -> Self {
        Self {
            id: UserId::new(),
            username,
            email,
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn email_is_normalised_to_lowercase() {
        let email = Email::parse("  Ada@Example.ORG ").unwrap();
        assert_eq!(email.as_str(), "ada@example.org");
    }

    #[test]
    fn email_rejects_malformed_input() {
        for bad in [
            "",
            "ada",
            "@example.org",
            "ada@",
            "ada@localhost",
            "a da@example.org",
            "a@@b.c",
        ] {
            assert!(
                Email::parse(bad).is_err(),
                "expected {bad:?} to be rejected"
            );
        }
    }

    #[test]
    fn email_deserialisation_goes_through_validation() {
        let err = serde_json::from_str::<Email>("\"not-an-email\"").unwrap_err();
        assert!(err.to_string().contains("invalid email address"));
    }

    #[test]
    fn username_enforces_charset_and_length() {
        assert!(Username::parse("ada_1").is_ok());
        assert!(Username::parse("ab").is_err());
        assert!(Username::parse("Ada").is_err());
        assert!(Username::parse("ada-lovelace").is_err());
        assert!(Username::parse("a".repeat(33)).is_err());
    }
}
