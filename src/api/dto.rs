use chrono::{DateTime, NaiveDate, Utc};
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
    pub balance: Decimal,
    pub details: Option<AccountDetailsDto>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
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
    pub subscription_id: Option<Uuid>,
    pub subscription_charge_id: Option<Uuid>,
}

#[derive(Serialize)]
pub struct PaginationInfo {
    pub total: i64,
    pub limit: i64,
    pub offset: i64,
}

#[derive(Serialize)]
pub struct TransactionListResponse {
    pub items: Vec<TransactionResponse>,
    pub pagination: PaginationInfo,
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
    pub from: Option<i64>,
    pub to: Option<i64>,
}

fn default_limit() -> i64 {
    50
}

// ── Monobank ──────────────────────────────────────────────────────────────

#[derive(Debug, serde::Deserialize)]
pub struct ConnectMonobankRequest {
    pub account_id: Uuid,
    pub token: crate::domain::secret::SecretString,
    pub external_account_id: String,
}

#[derive(Debug, serde::Serialize)]
pub struct MonobankConnectionResponse {
    pub id: Uuid,
    pub account_id: Uuid,
    pub provider: String,
    pub external_account_id: String,
    pub sync_status: String,
    pub last_synced_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, serde::Deserialize)]
pub struct ResyncQuery {
    pub from: i64,
    pub to: i64,
}

#[derive(Debug, serde::Serialize)]
pub struct ResyncJobResponse {
    pub connection_id: Uuid,
    pub sync_status: String,
    pub from: i64,
    pub to: i64,
    pub enqueued_at: DateTime<Utc>,
}

#[derive(Debug, serde::Serialize)]
pub struct MonoAccountResponse {
    pub id: String,
    pub currency_code: u16,
    pub balance: i64,
    pub account_type: String,
    pub iban: Option<String>,
}

// User Settings
#[derive(Serialize)]
pub struct UserSettingsResponse {
    pub base_currency: String,
}

#[derive(Deserialize)]
pub struct UpdateUserSettingsRequest {
    pub base_currency: String,
}

// ── Subscriptions ──────────────────────────────────────────────────────────

#[derive(Debug, serde::Serialize)]
pub struct EmailConnectionResponse {
    pub id: Uuid,
    pub email_address: String,
    pub provider: String,
    pub status: String,
    pub last_synced_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, serde::Serialize)]
pub struct GmailOAuthStartResponse {
    pub authorize_url: String,
    pub state: String,
}

#[derive(Debug, serde::Deserialize)]
pub struct GmailOAuthCallbackRequest {
    pub code: String,
    pub state: String,
}

#[derive(Debug, serde::Serialize)]
pub struct SubscriptionResponse {
    pub id: Uuid,
    pub provider: String,
    pub product_name: String,
    pub amount: Decimal,
    pub currency: String,
    pub billing_period: String,
    pub status: String,
    pub started_at: DateTime<Utc>,
    pub last_charged_at: Option<DateTime<Utc>>,
    pub next_expected_at: Option<DateTime<Utc>>,
    pub category_id: Option<Uuid>,
    pub overrides: SubscriptionOverridesResponse,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub charges: Option<Vec<SubscriptionChargeResponse>>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, serde::Serialize)]
pub struct SubscriptionOverridesResponse {
    pub product_name: Option<String>,
    pub billing_period: Option<String>,
    pub status: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
pub struct SubscriptionListQuery {
    pub status: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
pub struct UpdateSubscriptionRequest {
    #[serde(default, deserialize_with = "deserialize_optional_field")]
    pub product_name: Option<Option<String>>,
    #[serde(default, deserialize_with = "deserialize_optional_field")]
    pub billing_period: Option<Option<String>>,
    #[serde(default, deserialize_with = "deserialize_optional_field")]
    pub status: Option<Option<String>>,
    #[serde(default, deserialize_with = "deserialize_optional_field")]
    pub category_id: Option<Option<Uuid>>,
}

#[derive(Debug, serde::Serialize)]
pub struct SubscriptionChargeResponse {
    pub id: Uuid,
    pub subscription_id: Uuid,
    pub amount: Decimal,
    pub currency: String,
    pub charged_at: DateTime<Utc>,
    pub kind: String,
    pub transaction_id: Option<Uuid>,
    pub match_status: String,
}

#[derive(Debug, serde::Deserialize)]
pub struct LinkChargeRequest {
    pub transaction_id: Uuid,
}

#[derive(Debug, serde::Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum MarkTransactionSubscriptionRequest {
    Create {
        product_name: String,
        billing_period: String,
    },
    Attach {
        subscription_id: Uuid,
    },
}

#[derive(Debug, serde::Serialize)]
pub struct MarkTransactionSubscriptionResponse {
    pub subscription: SubscriptionResponse,
    pub charge: SubscriptionChargeResponse,
    pub subscription_created: bool,
}

#[derive(Debug, serde::Serialize)]
pub struct ForecastResponse {
    pub window_start: DateTime<Utc>,
    pub window_end: DateTime<Utc>,
    pub base_currency: String,
    pub base_total: Decimal,
    pub by_currency: std::collections::HashMap<String, Decimal>,
    pub monthly_equivalent_total: Decimal,
    pub yearly_equivalent_total: Decimal,
    pub normalized_by_currency:
        std::collections::HashMap<String, ForecastNormalizedCurrencyResponse>,
    pub fx_quotes: Vec<ForecastFxQuoteResponse>,
    pub complete: bool,
    pub warnings: Vec<String>,
}

#[derive(Debug, serde::Serialize)]
pub struct ForecastNormalizedCurrencyResponse {
    pub monthly: Decimal,
    pub yearly: Decimal,
}

#[derive(Debug, serde::Serialize)]
pub struct ForecastFxQuoteResponse {
    pub from_currency: String,
    pub to_currency: String,
    pub rate: Decimal,
    pub requested_date: NaiveDate,
    pub rate_date: NaiveDate,
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
