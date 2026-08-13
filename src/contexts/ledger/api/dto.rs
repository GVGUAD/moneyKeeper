use chrono::{DateTime, Utc};
use serde::Deserialize;
use uuid::Uuid;

use crate::contexts::ledger::public::{
    AccountKind, AccountNature, BudgetVisibility, ManualTransactionKind,
};

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MoneyRequest {
    pub(crate) amount: String,
    pub(crate) currency: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OpenAccountRequest {
    pub(crate) name: String,
    pub(crate) currency: String,
    pub(crate) kind: AccountKind,
    pub(crate) nature: AccountNature,
    pub(crate) opening_balance: String,
    pub(crate) occurred_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RenameAccountRequest {
    pub(crate) name: String,
    pub(crate) expected_version: i64,
    pub(crate) occurred_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExpectedAccountVersionRequest {
    pub(crate) expected_version: i64,
    pub(crate) occurred_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RecordTransactionRequest {
    pub(crate) account_id: Uuid,
    pub(crate) kind: ManualTransactionKind,
    pub(crate) amount: MoneyRequest,
    pub(crate) description: String,
    pub(crate) category_id: Option<Uuid>,
    #[serde(default)]
    pub(crate) note: Option<String>,
    #[serde(default)]
    pub(crate) tags: Vec<String>,
    #[serde(default = "included")]
    pub(crate) budget_visibility: BudgetVisibility,
    pub(crate) occurred_at: Option<DateTime<Utc>>,
}

fn included() -> BudgetVisibility {
    BudgetVisibility::Included
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AnnotationRequest {
    pub(crate) expected_version: i64,
    pub(crate) description: Option<String>,
    pub(crate) category_id: Option<Uuid>,
    #[serde(default)]
    pub(crate) clear_category: bool,
    pub(crate) note: Option<String>,
    #[serde(default)]
    pub(crate) clear_note: bool,
    pub(crate) tags: Option<Vec<String>>,
    pub(crate) budget_visibility: Option<BudgetVisibility>,
    pub(crate) occurred_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReverseRequest {
    pub(crate) reason: String,
    pub(crate) occurred_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReplaceRequest {
    pub(crate) account_id: Uuid,
    pub(crate) kind: ManualTransactionKind,
    pub(crate) amount: MoneyRequest,
    pub(crate) description: String,
    pub(crate) category_id: Option<Uuid>,
    pub(crate) note: Option<String>,
    #[serde(default)]
    pub(crate) tags: Vec<String>,
    #[serde(default = "included")]
    pub(crate) budget_visibility: BudgetVisibility,
    pub(crate) occurred_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TransferRequest {
    pub(crate) source_account_id: Uuid,
    pub(crate) target_account_id: Uuid,
    pub(crate) source_amount: MoneyRequest,
    pub(crate) target_amount: MoneyRequest,
    pub(crate) fee: Option<MoneyRequest>,
    pub(crate) implied_rate: Option<String>,
    pub(crate) description: String,
    pub(crate) occurred_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BalanceCorrectionRequest {
    pub(crate) target_display_balance: MoneyRequest,
    pub(crate) expected_balance_version: i64,
    pub(crate) reason: String,
    pub(crate) observed_at: DateTime<Utc>,
    pub(crate) occurred_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ActivityQuery {
    pub(crate) after_occurred_at: Option<DateTime<Utc>>,
    pub(crate) after_sequence: Option<i64>,
    pub(crate) limit: Option<u32>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ApproveReconciliationRequest {
    pub(crate) expected_version: i64,
    pub(crate) expected_balance_version: i64,
    pub(crate) reason: String,
    pub(crate) occurred_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DismissReconciliationRequest {
    pub(crate) expected_version: i64,
    pub(crate) reason: String,
    pub(crate) occurred_at: Option<DateTime<Utc>>,
}
