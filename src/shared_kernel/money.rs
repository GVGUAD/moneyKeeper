//! Exact, currency-safe monetary values.

use crate::shared_kernel::CurrencyCode;
use rust_decimal::Decimal;
use serde::Serialize;
use std::fmt;

const NUMERIC_28_8_MAX_MANTISSA: i128 = 9_999_999_999_999_999_999_999_999_999;

/// An exact monetary amount in one currency.
///
/// Construction checks the currency's resolved minor-unit scale and the
/// future PostgreSQL `NUMERIC(28,8)` storage bound. The type intentionally
/// implements outbound serialization only: inbound DTOs must resolve a
/// currency definition before invoking [`Money::new`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Money {
    #[serde(with = "rust_decimal::serde::str")]
    amount: Decimal,
    currency: CurrencyCode,
}

impl Money {
    /// Precision of the Finance V2 bounded PostgreSQL numeric representation.
    pub const DATABASE_PRECISION: u32 = 28;

    /// Scale of the Finance V2 bounded PostgreSQL numeric representation.
    pub const DATABASE_SCALE: u32 = 8;

    /// Constructs exact money using a Reference Data-resolved minor-unit scale.
    ///
    /// # Errors
    ///
    /// Returns an error when the minor-unit scale exceeds the storage scale,
    /// the amount has excess fractional digits, or it cannot fit in
    /// `NUMERIC(28,8)`.
    pub fn new(
        amount: Decimal,
        currency: CurrencyCode,
        minor_unit_scale: u32,
    ) -> Result<Self, MoneyError> {
        validate_minor_unit_scale(minor_unit_scale)?;
        if amount.scale() > minor_unit_scale {
            return Err(MoneyError::ExcessScale {
                actual: amount.scale(),
                allowed: minor_unit_scale,
            });
        }
        validate_database_bounds(amount)?;
        Ok(Self { amount, currency })
    }

    /// Constructs zero money through the same currency-scale validation path.
    ///
    /// # Errors
    ///
    /// Returns an error when `minor_unit_scale` exceeds the storage scale.
    pub fn zero(currency: CurrencyCode, minor_unit_scale: u32) -> Result<Self, MoneyError> {
        Self::new(Decimal::ZERO, currency, minor_unit_scale)
    }

    /// Returns the exact decimal amount.
    pub const fn amount(&self) -> Decimal {
        self.amount
    }

    /// Returns the monetary currency.
    pub const fn currency(&self) -> &CurrencyCode {
        &self.currency
    }

    /// Returns whether the amount is zero.
    pub fn is_zero(&self) -> bool {
        self.amount.is_zero()
    }

    /// Adds same-currency money without rounding.
    ///
    /// # Errors
    ///
    /// Returns an error for different currencies, Decimal arithmetic overflow,
    /// or a result outside the database numeric bound.
    pub fn checked_add(&self, rhs: &Self) -> Result<Self, MoneyError> {
        self.require_same_currency(rhs)?;
        let amount = self
            .amount
            .checked_add(rhs.amount)
            .ok_or(MoneyError::ArithmeticOverflow)?;
        Self::from_arithmetic(amount, self.currency.clone())
    }

    /// Subtracts same-currency money without rounding.
    ///
    /// # Errors
    ///
    /// Returns an error for different currencies, Decimal arithmetic overflow,
    /// or a result outside the database numeric bound.
    pub fn checked_sub(&self, rhs: &Self) -> Result<Self, MoneyError> {
        self.require_same_currency(rhs)?;
        let amount = self
            .amount
            .checked_sub(rhs.amount)
            .ok_or(MoneyError::ArithmeticOverflow)?;
        Self::from_arithmetic(amount, self.currency.clone())
    }

    /// Returns the exact additive inverse without rounding.
    ///
    /// # Errors
    ///
    /// Returns an error for Decimal arithmetic overflow or a result outside the
    /// database numeric bound.
    pub fn checked_neg(&self) -> Result<Self, MoneyError> {
        // `Decimal` stores its sign separately, so negation itself cannot
        // overflow. Keep the operation fallible to enforce the persistence
        // bound just like the other checked arithmetic operations.
        let amount = -self.amount;
        Self::from_arithmetic(amount, self.currency.clone())
    }

    fn require_same_currency(&self, rhs: &Self) -> Result<(), MoneyError> {
        if self.currency == rhs.currency {
            return Ok(());
        }
        Err(MoneyError::CurrencyMismatch {
            left: self.currency.clone(),
            right: rhs.currency.clone(),
        })
    }

    fn from_arithmetic(amount: Decimal, currency: CurrencyCode) -> Result<Self, MoneyError> {
        validate_database_bounds(amount)?;
        Ok(Self { amount, currency })
    }
}

impl fmt::Display for Money {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} {}", self.amount, self.currency)
    }
}

fn validate_minor_unit_scale(minor_unit_scale: u32) -> Result<(), MoneyError> {
    if minor_unit_scale > Money::DATABASE_SCALE {
        return Err(MoneyError::InvalidMinorUnitScale {
            scale: minor_unit_scale,
            max: Money::DATABASE_SCALE,
        });
    }
    Ok(())
}

fn validate_database_bounds(amount: Decimal) -> Result<(), MoneyError> {
    let maximum = Decimal::from_i128_with_scale(NUMERIC_28_8_MAX_MANTISSA, 8);
    if amount > maximum || amount < -maximum {
        return Err(MoneyError::OutOfBounds);
    }
    Ok(())
}

/// Explains why construction or arithmetic on money was rejected.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum MoneyError {
    /// The resolved currency definition supplied an unsupported scale.
    #[error("minor-unit scale {scale} exceeds maximum {max}")]
    InvalidMinorUnitScale {
        /// Rejected minor-unit scale.
        scale: u32,
        /// Maximum supported storage scale.
        max: u32,
    },
    /// The amount carried more fractional digits than the currency permits.
    #[error("money scale {actual} exceeds currency minor-unit scale {allowed}")]
    ExcessScale {
        /// Fractional scale of the rejected Decimal.
        actual: u32,
        /// Minor-unit scale allowed by the resolved currency definition.
        allowed: u32,
    },
    /// The amount cannot be stored in Finance V2's `NUMERIC(28,8)` columns.
    #[error("money amount is outside the NUMERIC(28,8) bound")]
    OutOfBounds,
    /// An operation attempted to mix two currencies.
    #[error("cannot combine money in {left} and {right}")]
    CurrencyMismatch {
        /// Currency of the left-hand value.
        left: CurrencyCode,
        /// Currency of the right-hand value.
        right: CurrencyCode,
    },
    /// Decimal arithmetic could not represent the result.
    #[error("money arithmetic overflowed")]
    ArithmeticOverflow,
}
