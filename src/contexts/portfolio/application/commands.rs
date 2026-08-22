//! Task-oriented Portfolio command DTOs.

use crate::{
    contexts::{ledger::public::LedgerAccountId, portfolio::domain::*},
    shared_kernel::{CorrelationId, CurrencyCode, IdempotencyKey, UserId},
};
use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, Serialize)]
pub struct CreateManualOvdpInstrument {
    pub user_id: UserId,
    pub identifier: InstrumentIdentifier,
    pub display_name: String,
    pub currency: CurrencyCode,
    pub face_value: Decimal,
    pub issue_date: NaiveDate,
    pub maturity_date: NaiveDate,
    pub coupon_terms: CouponTerms,
    pub idempotency_key: IdempotencyKey,
    pub correlation_id: CorrelationId,
    pub occurred_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Serialize)]
pub struct OpenPortfolioAccount {
    pub user_id: UserId,
    pub name: String,
    pub idempotency_key: IdempotencyKey,
    pub correlation_id: CorrelationId,
    pub occurred_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ChangePortfolioAccount {
    pub user_id: UserId,
    pub account_id: PortfolioAccountId,
    pub expected_version: u64,
    pub name: Option<String>,
    pub idempotency_key: IdempotencyKey,
    pub correlation_id: CorrelationId,
    pub occurred_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RequestedLotAllocation {
    pub lot_id: LotId,
    #[serde(with = "rust_decimal::serde::str")]
    pub quantity: Decimal,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PortfolioActivityCommand {
    OpeningPosition {
        quantity: Decimal,
        acquisition_cost: Option<Decimal>,
        acquisition_date: NaiveDate,
        reason: String,
    },
    Buy {
        quantity: Decimal,
        total_acquisition_cost: Decimal,
        fee: Option<Decimal>,
        accrued_interest: Option<Decimal>,
        trade_at: DateTime<Utc>,
    },
    Sell {
        quantity: Decimal,
        proceeds: Decimal,
        fee: Option<Decimal>,
        trade_at: DateTime<Utc>,
        lot_allocations: Option<Vec<RequestedLotAllocation>>,
    },
    Coupon {
        amount: Decimal,
        ex_date: Option<NaiveDate>,
        payment_date: NaiveDate,
    },
    Redemption {
        quantity: Decimal,
        proceeds: Decimal,
        maturity_date: NaiveDate,
        reference: String,
        lot_allocations: Option<Vec<RequestedLotAllocation>>,
    },
    PositionCorrection {
        quantity_delta: Decimal,
        cost_delta: Option<Decimal>,
        reason: String,
        effective_at: DateTime<Utc>,
    },
}

#[derive(Clone, Debug, Serialize)]
pub struct OptionalCashSettlement {
    pub cash_account_id: LedgerAccountId,
    pub amount: Decimal,
}

#[derive(Clone, Debug, Serialize)]
pub struct RecordPortfolioTransaction {
    pub user_id: UserId,
    pub account_id: PortfolioAccountId,
    pub instrument_id: InstrumentId,
    pub expected_account_version: u64,
    pub expected_position_version: u64,
    pub activity: PortfolioActivityCommand,
    pub cash_settlement: Option<OptionalCashSettlement>,
    pub actor_id: PortfolioActorId,
    pub idempotency_key: IdempotencyKey,
    pub correlation_id: CorrelationId,
    pub recorded_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ReversePortfolioTransaction {
    pub user_id: UserId,
    pub transaction_id: PortfolioTransactionId,
    pub expected_account_version: u64,
    pub expected_position_version: u64,
    pub reason: String,
    pub actor_id: PortfolioActorId,
    pub idempotency_key: IdempotencyKey,
    pub correlation_id: CorrelationId,
    pub recorded_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Serialize)]
pub struct RecordValuationSnapshot {
    pub user_id: UserId,
    pub account_id: PortfolioAccountId,
    pub instrument_id: InstrumentId,
    pub price_per_instrument: Decimal,
    pub accrued_interest_per_instrument: Decimal,
    pub currency: CurrencyCode,
    pub source: String,
    pub quoted_at: DateTime<Utc>,
    pub idempotency_key: IdempotencyKey,
    pub correlation_id: CorrelationId,
    pub recorded_at: DateTime<Utc>,
}

pub(crate) fn canonical_request_hash<T: Serialize>(
    scope: &str,
    user: UserId,
    value: &T,
) -> Result<[u8; 32], serde_json::Error> {
    let value = serde_json::to_value((scope, user, value))?;
    let bytes = serde_json::to_vec(&canonical(value))?;
    Ok(Sha256::digest(bytes).into())
}
fn canonical(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let mut entries = map.into_iter().collect::<Vec<_>>();
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            serde_json::Value::Object(
                entries
                    .into_iter()
                    .map(|(k, v)| (k, canonical(v)))
                    .collect(),
            )
        }
        serde_json::Value::Array(v) => {
            serde_json::Value::Array(v.into_iter().map(canonical).collect())
        }
        v => v,
    }
}
