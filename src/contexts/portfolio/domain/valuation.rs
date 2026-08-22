//! Append-only manual valuation snapshot.

use super::{InstrumentId, PortfolioAccountId, PortfolioError};
use crate::shared_kernel::{CurrencyCode, UserId};
use chrono::{DateTime, Utc};
use rust_decimal::{Decimal, RoundingStrategy};
use serde::{Deserialize, Serialize};

crate::define_uuid_id!(#[doc="Identifies an immutable Portfolio valuation snapshot."] pub ValuationSnapshotId);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuoteConvention {
    AbsolutePerInstrument,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValuationSnapshot {
    pub id: ValuationSnapshotId,
    pub user_id: UserId,
    pub account_id: PortfolioAccountId,
    pub instrument_id: InstrumentId,
    pub price_per_instrument: Decimal,
    pub accrued_interest_per_instrument: Decimal,
    pub currency: CurrencyCode,
    pub quote_convention: QuoteConvention,
    pub source: String,
    pub quoted_at: DateTime<Utc>,
    pub recorded_at: DateTime<Utc>,
}

impl ValuationSnapshot {
    #[allow(clippy::too_many_arguments)]
    pub fn record(
        user_id: UserId,
        account_id: PortfolioAccountId,
        instrument_id: InstrumentId,
        price_per_instrument: Decimal,
        accrued_interest_per_instrument: Decimal,
        currency: CurrencyCode,
        source: impl Into<String>,
        quoted_at: DateTime<Utc>,
        recorded_at: DateTime<Utc>,
    ) -> Result<Self, PortfolioError> {
        if price_per_instrument <= Decimal::ZERO {
            return Err(PortfolioError::InvalidValue("price_per_instrument"));
        }
        if accrued_interest_per_instrument < Decimal::ZERO {
            return Err(PortfolioError::InvalidValue(
                "accrued_interest_per_instrument",
            ));
        }
        let source = source.into();
        if source.is_empty() || source.trim() != source {
            return Err(PortfolioError::InvalidValue("valuation_source"));
        }
        Ok(Self {
            id: ValuationSnapshotId::generate(),
            user_id,
            account_id,
            instrument_id,
            price_per_instrument,
            accrued_interest_per_instrument,
            currency,
            quote_convention: QuoteConvention::AbsolutePerInstrument,
            source,
            quoted_at,
            recorded_at,
        })
    }
    pub fn market_value(
        &self,
        quantity: Decimal,
        currency_scale: u32,
    ) -> Result<Decimal, PortfolioError> {
        quantity
            .checked_mul(
                self.price_per_instrument
                    .checked_add(self.accrued_interest_per_instrument)
                    .ok_or(PortfolioError::Arithmetic)?,
            )
            .ok_or(PortfolioError::Arithmetic)
            .map(|v| {
                v.round_dp_with_strategy(currency_scale, RoundingStrategy::MidpointNearestEven)
            })
    }
}
