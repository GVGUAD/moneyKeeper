use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::Deserialize;

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
