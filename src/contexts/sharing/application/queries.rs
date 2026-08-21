//! Stable Sharing read models.

use crate::contexts::sharing::domain::{
    BillSplitId, BillStatus, BillVersion, Contact, ContactId, ContactStatus, ContactVersion,
    SettlementId, SettlementStatus, SettlementVersion,
};
use crate::shared_kernel::{CorrelationId, CurrencyCode, UserId};
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContactView {
    pub id: ContactId,
    pub user_id: UserId,
    pub display_name: String,
    pub note: Option<String>,
    pub status: ContactStatus,
    pub version: ContactVersion,
}
impl From<&Contact> for ContactView {
    fn from(value: &Contact) -> Self {
        Self {
            id: value.id(),
            user_id: value.user_id(),
            display_name: value.name().as_str().to_owned(),
            note: value.note().map(ToOwned::to_owned),
            status: value.status(),
            version: value.version(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContactResult {
    pub contact: ContactView,
    pub replayed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessView {
    pub state: String,
    pub correlation_id: CorrelationId,
    pub last_error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BillView {
    pub id: BillSplitId,
    pub user_id: UserId,
    pub title: String,
    pub occurred_at: DateTime<Utc>,
    #[serde(with = "rust_decimal::serde::str")]
    pub total: Decimal,
    pub currency: CurrencyCode,
    pub current_revision: u32,
    pub status: BillStatus,
    pub version: BillVersion,
    pub active_settlements: u32,
    pub allocations: serde_json::Value,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BillResult {
    pub bill: BillView,
    pub process: ProcessView,
    pub replayed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettlementView {
    pub id: SettlementId,
    pub bill_id: BillSplitId,
    #[serde(with = "rust_decimal::serde::str")]
    pub amount: Decimal,
    pub currency: CurrencyCode,
    pub status: SettlementStatus,
    pub version: SettlementVersion,
    pub process: ProcessView,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettlementResult {
    pub settlement: SettlementView,
    pub replayed: bool,
}
