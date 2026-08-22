//! Immutable Portfolio read DTOs.

use crate::{
    contexts::portfolio::domain::*,
    shared_kernel::{CorrelationId, CurrencyCode},
};
use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct InstrumentView {
    pub id: InstrumentId,
    pub identifier: InstrumentIdentifier,
    pub display_name: String,
    pub currency: CurrencyCode,
    #[serde(with = "rust_decimal::serde::str")]
    pub face_value: Decimal,
    pub issue_date: NaiveDate,
    pub maturity_date: NaiveDate,
    pub coupon_terms: CouponTerms,
    pub version: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PortfolioAccountView {
    pub id: PortfolioAccountId,
    pub name: String,
    pub lifecycle: AccountLifecycle,
    pub version: u64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PositionView {
    pub account_id: PortfolioAccountId,
    pub instrument_id: InstrumentId,
    #[serde(with = "rust_decimal::serde::str")]
    pub quantity: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub known_cost_quantity: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub unknown_cost_quantity: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub remaining_known_cost: Decimal,
    pub realized_gain_loss: Option<Decimal>,
    pub currency: CurrencyCode,
    pub version: u64,
    pub latest_market_value: Option<Decimal>,
    pub valuation_as_of: Option<DateTime<Utc>>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PortfolioTransactionView {
    pub id: PortfolioTransactionId,
    pub account_id: PortfolioAccountId,
    pub instrument_id: InstrumentId,
    pub kind: PortfolioTransactionKind,
    #[serde(with = "rust_decimal::serde::str")]
    pub quantity: Decimal,
    pub currency: CurrencyCode,
    pub reversal_of: Option<PortfolioTransactionId>,
    pub correlation_id: CorrelationId,
    pub effective_at: DateTime<Utc>,
    pub recorded_at: DateTime<Utc>,
    pub cash_accounting_status: CashAccountingStatus,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ValuationView {
    pub id: ValuationSnapshotId,
    pub account_id: PortfolioAccountId,
    pub instrument_id: InstrumentId,
    #[serde(with = "rust_decimal::serde::str")]
    pub price_per_instrument: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub accrued_interest_per_instrument: Decimal,
    pub currency: CurrencyCode,
    pub source: String,
    pub quoted_at: DateTime<Utc>,
    pub recorded_at: DateTime<Utc>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CashAccountingStatus {
    NotRequested,
    Pending,
    Posted,
    Retrying,
    Failed,
    CancelledNoFinancialEffect,
    Reversed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortfolioCommandResult {
    pub aggregate_id: uuid::Uuid,
    pub transaction_id: Option<PortfolioTransactionId>,
    pub version: u64,
    pub status: String,
    pub cash_accounting_status: CashAccountingStatus,
    pub correlation_id: CorrelationId,
    pub replayed: bool,
}
