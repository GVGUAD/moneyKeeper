use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Deserializer, Serialize};
use uuid::Uuid;

/// Deserializer for `Option<Option<T>>` that distinguishes between a missing field (`None`)
/// and an explicit `null` (`Some(None)`).  Without this, serde maps both to `None`.
fn deserialize_optional_field<'de, T, D>(d: D) -> Result<Option<Option<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Ok(Some(Option::deserialize(d)?))
}

// Auth
#[derive(Deserialize)]
pub struct RegisterRequest {
    pub email: String,
    pub password: String,
}

#[derive(Deserialize)]
pub struct LoginRequest {
    pub email: String,
    pub password: String,
}

#[derive(Deserialize)]
pub struct RefreshRequest {
    pub refresh_token: String,
}

#[derive(Deserialize)]
pub struct LogoutRequest {
    pub refresh_token: String,
}

#[derive(Serialize)]
pub struct AuthResponse {
    pub access_token: String,
    pub refresh_token: String,
    pub user_id: Uuid,
}

// Accounts
#[derive(Deserialize)]
pub struct CreateAccountRequest {
    pub name: String,
    pub account_type: String,
    pub currency: String,
    pub details: Option<AccountDetailsDto>,
}

#[derive(Deserialize)]
pub struct UpdateAccountRequest {
    pub name: Option<String>,
    pub currency: Option<String>,
    #[allow(dead_code)]
    pub details: Option<AccountDetailsDto>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AccountDetailsDto {
    Savings {
        interest_rate: Decimal,
        compounding_period: String,
    },
    Loan {
        counterparty: String,
        direction: String,
        interest_rate: Option<Decimal>,
        due_date: Option<String>,
    },
    Investment {
        broker: Option<String>,
    },
    Binance {
        label: Option<String>,
    },
}

#[derive(Serialize)]
pub struct AccountResponse {
    pub id: Uuid,
    pub name: String,
    pub account_type: String,
    pub currency: String,
    pub details: Option<AccountDetailsDto>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Serialize)]
pub struct BalanceResponse {
    pub account_id: Uuid,
    pub balance: Decimal,
    pub currency: String,
}

// Transactions
#[derive(Deserialize)]
pub struct CreateTransactionRequest {
    pub amount: Decimal,
    pub currency: String,
    pub kind: String,
    pub category_id: Option<Uuid>,
    pub note: Option<String>,
    pub transacted_at: DateTime<Utc>,
    pub details: Option<TransactionDetailsDto>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TransactionDetailsDto {
    Trade {
        ticker: String,
        quantity: Decimal,
        price_per_unit: Option<Decimal>,
        fee: Option<Decimal>,
    },
    Transfer {
        to_account_id: Uuid,
    },
}

#[derive(Serialize)]
pub struct TransactionResponse {
    pub id: Uuid,
    pub account_id: Uuid,
    pub amount: Decimal,
    pub currency: String,
    pub kind: String,
    pub category_id: Option<Uuid>,
    pub note: Option<String>,
    pub transacted_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub details: Option<TransactionDetailsDto>,
}

// Categories
#[derive(Deserialize)]
pub struct CreateCategoryRequest {
    pub name: String,
    pub color: Option<String>,
}

#[derive(Deserialize)]
pub struct UpdateCategoryRequest {
    pub name: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_field")]
    pub color: Option<Option<String>>,
}

#[derive(Serialize)]
pub struct CategoryResponse {
    pub id: Uuid,
    pub name: String,
    pub color: Option<String>,
    pub created_at: DateTime<Utc>,
}

// Pagination
#[derive(Deserialize)]
pub struct TxListQuery {
    pub kind: Option<String>,
    pub category_id: Option<Uuid>,
    #[serde(default = "default_limit")]
    pub limit: i64,
    #[serde(default)]
    pub offset: i64,
}

fn default_limit() -> i64 {
    50
}

// ── Monobank ──────────────────────────────────────────────────────────────

#[derive(Debug, serde::Deserialize)]
pub struct ConnectMonobankRequest {
    pub account_id: uuid::Uuid,
    pub token: String,
    pub monobank_account_id: String,
}

#[derive(Debug, serde::Serialize)]
pub struct MonobankConnectionResponse {
    pub id: uuid::Uuid,
    pub account_id: uuid::Uuid,
    pub monobank_account_id: String,
    pub sync_status: String,
    pub last_synced_at: Option<chrono::DateTime<chrono::Utc>>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, serde::Serialize)]
pub struct MonoAccountResponse {
    pub id: String,
    pub currency_code: u16,
    pub balance: i64,
    pub account_type: String,
    pub iban: Option<String>,
}

/// Webhook payload from Monobank
#[derive(Debug, serde::Deserialize)]
pub struct MonobankWebhookPayload {
    #[serde(rename = "type")]
    pub event_type: String,
    pub data: Option<MonobankWebhookData>,
}

#[derive(Debug, serde::Deserialize)]
pub struct MonobankWebhookData {
    pub account: String,
    #[serde(rename = "statementItem")]
    pub statement_item: crate::domain::monobank::MonoStatementItem,
}
