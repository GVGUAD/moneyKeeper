use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct AccountBody {
    pub name: String,
    pub expected_version: Option<u64>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct VersionBody {
    pub expected_version: u64,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct OvdpBody {
    pub identifier_kind: String,
    pub identifier: String,
    pub display_name: String,
    pub currency: String,
    pub face_value: String,
    pub issue_date: NaiveDate,
    pub maturity_date: NaiveDate,
    pub coupon_kind: String,
    pub coupon_rate: Option<String>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct TransactionBody {
    pub portfolio_account_id: Uuid,
    pub instrument_id: Uuid,
    pub expected_account_version: u64,
    pub expected_position_version: u64,
    pub kind: String,
    pub quantity: Option<String>,
    pub acquisition_cost: Option<String>,
    pub proceeds: Option<String>,
    pub amount: Option<String>,
    pub fee: Option<String>,
    pub accrued_interest: Option<String>,
    pub effective_at: Option<DateTime<Utc>>,
    pub effective_date: Option<NaiveDate>,
    pub reason: Option<String>,
    pub lot_allocations: Option<Vec<LotAllocationBody>>,
    pub cash_account_id: Option<Uuid>,
    pub cash_amount: Option<String>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct LotAllocationBody {
    pub lot_id: Uuid,
    pub quantity: String,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct ReversalBody {
    pub expected_account_version: u64,
    pub expected_position_version: u64,
    pub reason: String,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct ValuationBody {
    pub portfolio_account_id: Uuid,
    pub instrument_id: Uuid,
    pub price_per_instrument: String,
    pub accrued_interest_per_instrument: String,
    pub currency: String,
    pub source: String,
    pub quoted_at: DateTime<Utc>,
}
#[derive(Clone, Debug, Deserialize)]
pub(crate) struct PositionParams {
    pub portfolio_account_id: Uuid,
}
#[derive(Clone, Debug, Deserialize)]
pub(crate) struct ValuationParams {
    pub portfolio_account_id: Uuid,
    pub instrument_id: Uuid,
}
