use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use rust_decimal::RoundingStrategy;

use crate::shared_kernel::CurrencyCode;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CurrencyDefinition {
    pub(crate) code: CurrencyCode,
    pub(crate) numeric_code: Option<String>,
    pub(crate) name: String,
    pub(crate) minor_unit: u8,
    pub(crate) enabled: bool,
    pub(crate) updated_at: DateTime<Utc>,
}

/// Exact meaning: one unit of `base` equals `rate` units of `quote`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExchangeRate {
    base: CurrencyCode,
    quote: CurrencyCode,
    rate: Decimal,
}
impl ExchangeRate {
    pub fn new(base: CurrencyCode, quote: CurrencyCode, rate: Decimal) -> Result<Self, FxError> {
        if base == quote {
            return Err(FxError::SameCurrency);
        }
        if rate <= Decimal::ZERO {
            return Err(FxError::NonPositive);
        }
        if rate.scale() > 12 {
            return Err(FxError::ExcessScale);
        }
        Ok(Self { base, quote, rate })
    }
    pub fn invert(&self) -> Result<Self, FxError> {
        let rate = Decimal::ONE
            .checked_div(self.rate)
            .ok_or(FxError::Arithmetic)?
            .round_dp_with_strategy(12, RoundingStrategy::MidpointAwayFromZero);
        Self::new(self.quote.clone(), self.base.clone(), rate)
    }
    pub fn triangulate(&self, next: &Self) -> Result<Self, FxError> {
        if self.quote != next.base {
            return Err(FxError::CurrencyChain);
        }
        let rate = self
            .rate
            .checked_mul(next.rate)
            .ok_or(FxError::Arithmetic)?
            .round_dp_with_strategy(12, RoundingStrategy::MidpointAwayFromZero);
        Self::new(self.base.clone(), next.quote.clone(), rate)
    }
    pub const fn base(&self) -> &CurrencyCode {
        &self.base
    }
    pub const fn quote(&self) -> &CurrencyCode {
        &self.quote
    }
    pub const fn rate(&self) -> Decimal {
        self.rate
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum FxError {
    #[error("base and quote currencies must differ")]
    SameCurrency,
    #[error("exchange rate must be positive")]
    NonPositive,
    #[error("exchange rate scale exceeds 12")]
    ExcessScale,
    #[error("exchange-rate arithmetic failed")]
    Arithmetic,
    #[error("exchange-rate currencies do not form a chain")]
    CurrencyChain,
}
