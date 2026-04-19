use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::Deserialize;
use uuid::Uuid;

/// A single Monobank account (card) from client-info response.
#[derive(Debug, Clone, Deserialize)]
pub struct MonoAccount {
    pub id: String,
    #[serde(rename = "currencyCode")]
    pub currency_code: u16,
    pub balance: i64,
    #[serde(rename = "creditLimit")]
    pub credit_limit: i64,
    #[serde(rename = "type")]
    pub account_type: String,
    pub iban: Option<String>,
}

/// A single transaction from Monobank statement.
#[derive(Debug, Clone, Deserialize)]
pub struct MonoStatementItem {
    pub id: String,
    pub time: i64,
    pub description: Option<String>,
    pub mcc: i32,
    pub amount: i64,
    #[serde(rename = "operationAmount")]
    pub operation_amount: i64,
    #[serde(rename = "currencyCode")]
    pub currency_code: u16,
    pub balance: i64,
    pub hold: bool,
}

impl MonoStatementItem {
    /// Convert amount in kopecks (1/100 UAH) to Decimal.
    pub fn amount_decimal(&self) -> Decimal {
        Decimal::new(self.amount.abs(), 2)
    }

    /// true = income, false = expense
    pub fn is_income(&self) -> bool {
        self.amount > 0
    }

    pub fn transacted_at(&self) -> DateTime<Utc> {
        DateTime::from_timestamp(self.time, 0).unwrap_or_else(Utc::now)
    }
}

#[async_trait::async_trait]
pub trait MonobankApiClient: Send + Sync {
    /// Fetch client info and available accounts.
    async fn get_accounts(&self, token: &str) -> anyhow::Result<Vec<MonoAccount>>;

    /// Fetch statement for a date range (max 31 days per call, rate-limited to 1/min).
    async fn get_statement(
        &self,
        token: &str,
        account_id: &str,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> anyhow::Result<Vec<MonoStatementItem>>;

    /// Register a webhook URL for the given token.
    async fn set_webhook(&self, token: &str, webhook_url: &str) -> anyhow::Result<()>;
}

#[derive(Debug, Clone, PartialEq)]
pub enum SyncStatus {
    Pending,
    Syncing,
    Completed,
    Failed,
}

impl SyncStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            SyncStatus::Pending => "pending",
            SyncStatus::Syncing => "syncing",
            SyncStatus::Completed => "completed",
            SyncStatus::Failed => "failed",
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> anyhow::Result<Self> {
        match s {
            "pending" => Ok(SyncStatus::Pending),
            "syncing" => Ok(SyncStatus::Syncing),
            "completed" => Ok(SyncStatus::Completed),
            "failed" => Ok(SyncStatus::Failed),
            other => Err(anyhow::anyhow!("unknown sync_status: {other}")),
        }
    }
}

#[derive(Debug, Clone)]
pub struct MonobankConnection {
    pub id: Uuid,
    pub account_id: Uuid,
    pub user_id: Uuid,
    pub token: String,
    pub monobank_account_id: String,
    pub sync_status: SyncStatus,
    pub last_synced_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

impl MonobankConnection {
    pub fn new(
        account_id: Uuid,
        user_id: Uuid,
        token: String,
        monobank_account_id: String,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            account_id,
            user_id,
            token,
            monobank_account_id,
            sync_status: SyncStatus::Pending,
            last_synced_at: None,
            created_at: Utc::now(),
        }
    }
}

#[async_trait::async_trait]
pub trait MonobankConnectionRepository: Send + Sync {
    async fn create(&self, conn: &MonobankConnection) -> anyhow::Result<()>;
    async fn find_by_id(
        &self,
        id: Uuid,
        user_id: Uuid,
    ) -> anyhow::Result<Option<MonobankConnection>>;
    async fn find_by_monobank_account_id(
        &self,
        monobank_account_id: &str,
    ) -> anyhow::Result<Option<MonobankConnection>>;
    async fn list_by_user(&self, user_id: Uuid) -> anyhow::Result<Vec<MonobankConnection>>;
    async fn list_incomplete(&self) -> anyhow::Result<Vec<MonobankConnection>>;
    async fn update_status(
        &self,
        id: Uuid,
        status: SyncStatus,
        last_synced_at: Option<DateTime<Utc>>,
    ) -> anyhow::Result<()>;
    async fn delete(&self, id: Uuid, user_id: Uuid) -> anyhow::Result<()>;
}
