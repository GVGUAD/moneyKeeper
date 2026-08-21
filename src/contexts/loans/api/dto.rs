//! Decimal-string HTTP DTOs for loan tasks.

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct OpenLoanBody {
    pub direction: String,
    pub counterparty: String,
    pub contractual_principal: String,
    pub currency: String,
    pub start_date: NaiveDate,
    pub due_date: Option<NaiveDate>,
    pub annual_rate: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct ReviseTermsBody {
    pub expected_version: u64,
    pub counterparty: String,
    pub contractual_principal: String,
    pub currency: String,
    pub start_date: NaiveDate,
    pub due_date: Option<NaiveDate>,
    pub annual_rate: Option<String>,
    pub reason: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct MovementBody {
    pub expected_version: u64,
    pub currency: String,
    #[serde(default = "zero")]
    pub principal: String,
    #[serde(default = "zero")]
    pub accrued_interest: String,
    #[serde(default = "zero")]
    pub accrued_fee: String,
    #[serde(default = "zero")]
    pub current_interest: String,
    #[serde(default = "zero")]
    pub current_fee: String,
    pub cash_account_id: Option<Uuid>,
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct ClosureBody {
    pub expected_version: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct ReversalBody {
    pub expected_version: u64,
    pub reason: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct ReplacementBody {
    pub expected_version: u64,
    pub kind: String,
    pub currency: String,
    #[serde(default = "zero")]
    pub principal: String,
    #[serde(default = "zero")]
    pub accrued_interest: String,
    #[serde(default = "zero")]
    pub accrued_fee: String,
    #[serde(default = "zero")]
    pub current_interest: String,
    #[serde(default = "zero")]
    pub current_fee: String,
    pub cash_account_id: Option<Uuid>,
    pub reason: Option<String>,
}

fn zero() -> String {
    "0".to_owned()
}
