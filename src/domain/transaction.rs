use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq)]
pub enum TransactionKind {
    Income,
    Expense,
    Transfer,
    Buy,
    Sell,
    StakingReward,
}

impl TransactionKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Income => "Income",
            Self::Expense => "Expense",
            Self::Transfer => "Transfer",
            Self::Buy => "Buy",
            Self::Sell => "Sell",
            Self::StakingReward => "StakingReward",
        }
    }
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> anyhow::Result<Self> {
        match s {
            "Income" => Ok(Self::Income),
            "Expense" => Ok(Self::Expense),
            "Transfer" => Ok(Self::Transfer),
            "Buy" => Ok(Self::Buy),
            "Sell" => Ok(Self::Sell),
            "StakingReward" => Ok(Self::StakingReward),
            other => anyhow::bail!("unknown transaction kind: {other}"),
        }
    }
    pub fn affects_balance_positively(&self) -> bool {
        matches!(self, Self::Income | Self::Sell | Self::StakingReward)
    }
}

#[derive(Debug, Clone)]
pub struct Transaction {
    pub id: Uuid,
    pub account_id: Uuid,
    pub user_id: Uuid,
    pub amount: Decimal,
    pub currency: String,
    pub kind: TransactionKind,
    pub category_id: Option<Uuid>,
    pub note: Option<String>,
    pub external_id: Option<String>,
    /// Running account balance as reported by the external provider (e.g. Monobank)
    /// AFTER this transaction. `None` for manually-entered transactions.
    pub external_balance: Option<Decimal>,
    pub transacted_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}

impl Transaction {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        account_id: Uuid,
        user_id: Uuid,
        amount: Decimal,
        currency: String,
        kind: TransactionKind,
        category_id: Option<Uuid>,
        note: Option<String>,
        transacted_at: DateTime<Utc>,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            account_id,
            user_id,
            amount,
            currency,
            kind,
            category_id,
            note,
            external_id: None,
            external_balance: None,
            transacted_at,
            created_at: Utc::now(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct TransferLink {
    pub from_transaction_id: Uuid,
    pub to_transaction_id: Uuid,
}

#[derive(Debug, Clone)]
pub struct TradeDetails {
    pub transaction_id: Uuid,
    pub ticker: String,
    pub quantity: Decimal,
    pub price_per_unit: Option<Decimal>,
    pub fee: Option<Decimal>,
}

#[derive(Debug, Clone)]
pub enum TransactionDetails {
    Transfer(TransferLink),
    Trade(TradeDetails),
    None,
}

#[derive(Debug, Clone)]
pub struct TransactionListParams {
    pub account_id: Option<Uuid>,
    pub user_id: Uuid,
    pub kind: Option<TransactionKind>,
    pub category_id: Option<Uuid>,
    pub from: Option<DateTime<Utc>>,
    pub to: Option<DateTime<Utc>>,
    pub limit: i64,
    pub offset: i64,
}

#[async_trait::async_trait]
pub trait TransactionRepository: Send + Sync {
    async fn create(&self, tx: &Transaction, details: &TransactionDetails) -> anyhow::Result<()>;
    async fn find_by_id(
        &self,
        id: Uuid,
        user_id: Uuid,
    ) -> anyhow::Result<Option<(Transaction, TransactionDetails)>>;
    async fn list(
        &self,
        params: &TransactionListParams,
    ) -> anyhow::Result<Vec<(Transaction, TransactionDetails)>>;
    async fn count(&self, params: &TransactionListParams) -> anyhow::Result<i64>;
    #[allow(dead_code)]
    async fn update(&self, tx: &Transaction, details: &TransactionDetails) -> anyhow::Result<()>;
    async fn delete(&self, id: Uuid, user_id: Uuid) -> anyhow::Result<()>;
    /// Insert a transaction using INSERT OR IGNORE (for external syncs).
    /// Returns true if the row was actually inserted (false = already existed).
    async fn create_idempotent(&self, tx: &Transaction) -> anyhow::Result<bool>;
    /// Returns unlinked expense transactions in the inclusive time window,
    /// across all currencies, excluding candidates the user previously
    /// rejected for this charge. Amount/FX tolerance is applied in the
    /// application layer because each candidate may use a different currency.
    async fn list_unlinked_expense_candidates(
        &self,
        charge_id: Uuid,
        user_id: Uuid,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> anyhow::Result<Vec<Transaction>>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transaction_kind_roundtrip() {
        for k in [
            TransactionKind::Income,
            TransactionKind::Expense,
            TransactionKind::Transfer,
            TransactionKind::Buy,
            TransactionKind::Sell,
            TransactionKind::StakingReward,
        ] {
            assert_eq!(TransactionKind::from_str(k.as_str()).unwrap(), k);
        }
    }

    #[test]
    fn income_affects_balance_positively() {
        assert!(TransactionKind::Income.affects_balance_positively());
        assert!(!TransactionKind::Expense.affects_balance_positively());
    }
}
