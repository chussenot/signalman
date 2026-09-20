use std::fmt;
use std::ops::Add;

use std::borrow::Cow;

use serde::{Deserialize, Serialize};
use utoipa::openapi::RefOr;
use utoipa::openapi::schema::{KnownFormat, ObjectBuilder, Schema, SchemaFormat, SchemaType, Type};
use utoipa::{PartialSchema, ToSchema};

use super::DomainError;

/// A non-negative monetary amount in minor units (cents).
///
/// Integer minor units avoid floating-point rounding. The type only exposes
/// checked arithmetic, so overflow is an explicit error rather than a silent
/// wrap or a debug-only panic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "i64", into = "i64")]
pub struct Money(i64);

// Hand-written schema so the OpenAPI contract carries the `minimum: 0` bound
// that the derive macro cannot express on a newtype.
impl PartialSchema for Money {
    fn schema() -> RefOr<Schema> {
        ObjectBuilder::new()
            .schema_type(SchemaType::Type(Type::Integer))
            .format(Some(SchemaFormat::KnownFormat(KnownFormat::Int64)))
            .minimum(Some(0))
            .description(Some("Non-negative amount in minor units (cents)"))
            .examples([serde_json::json!(1999)])
            .into()
    }
}

impl ToSchema for Money {
    fn name() -> Cow<'static, str> {
        Cow::Borrowed("Money")
    }
}

impl Money {
    /// Zero amount.
    pub const ZERO: Self = Self(0);

    /// Construct from minor units, rejecting negative values.
    pub fn from_minor(minor: i64) -> Result<Self, DomainError> {
        if minor < 0 {
            return Err(DomainError::InvalidAmount(
                "amount must not be negative".into(),
            ));
        }
        Ok(Self(minor))
    }

    /// The amount in minor units.
    pub const fn minor(self) -> i64 {
        self.0
    }

    /// Add two amounts, returning an error on overflow.
    pub fn checked_add(self, other: Self) -> Result<Self, DomainError> {
        self.0
            .checked_add(other.0)
            .map(Self)
            .ok_or_else(|| DomainError::InvalidAmount("amount overflow".into()))
    }

    /// True when the amount is zero.
    pub const fn is_zero(self) -> bool {
        self.0 == 0
    }
}

impl TryFrom<i64> for Money {
    type Error = DomainError;

    fn try_from(value: i64) -> Result<Self, Self::Error> {
        Self::from_minor(value)
    }
}

impl From<Money> for i64 {
    fn from(value: Money) -> Self {
        value.0
    }
}

impl Add for Money {
    type Output = Result<Self, DomainError>;

    fn add(self, rhs: Self) -> Self::Output {
        self.checked_add(rhs)
    }
}

impl fmt::Display for Money {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{:02}", self.0 / 100, self.0 % 100)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn rejects_negative_amounts() {
        assert!(Money::from_minor(-1).is_err());
        assert!(serde_json::from_str::<Money>("-5").is_err());
    }

    #[test]
    fn addition_is_checked() {
        let a = Money::from_minor(i64::MAX).unwrap();
        let b = Money::from_minor(1).unwrap();
        assert!(a.checked_add(b).is_err());
        assert_eq!(
            (Money::from_minor(150).unwrap() + Money::from_minor(50).unwrap())
                .unwrap()
                .minor(),
            200
        );
    }

    #[test]
    fn displays_as_decimal() {
        assert_eq!(Money::from_minor(1999).unwrap().to_string(), "19.99");
        assert_eq!(Money::from_minor(5).unwrap().to_string(), "0.05");
    }
}
