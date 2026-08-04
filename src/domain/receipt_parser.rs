use chrono::{DateTime, Utc};
use rust_decimal::Decimal;

use crate::domain::email::RawEmail;
use crate::domain::subscription::{BillingPeriod, SubscriptionProvider};
use crate::domain::subscription_charge::ReceiptKind;

#[derive(Debug, Clone)]
pub struct ParsedReceipt {
    pub provider: SubscriptionProvider,
    pub product_name: String,
    pub merchant_key: String,
    pub amount: Decimal,
    pub currency: String,
    pub charged_at: DateTime<Utc>,
    pub billing_period_hint: Option<BillingPeriod>,
    pub kind: ReceiptKind,
}

pub trait ReceiptParser: Send + Sync {
    fn matches_sender(&self, from: &str) -> bool;
    fn parse(&self, email: &RawEmail) -> anyhow::Result<Option<ParsedReceipt>>;
}
