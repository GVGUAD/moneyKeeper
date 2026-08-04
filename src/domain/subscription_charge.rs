use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq)]
pub enum ChargeMatchStatus {
    Pending,
    Matched,
    Unmatched,
}

impl ChargeMatchStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "Pending",
            Self::Matched => "Matched",
            Self::Unmatched => "Unmatched",
        }
    }
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> anyhow::Result<Self> {
        match s {
            "Pending" => Ok(Self::Pending),
            "Matched" => Ok(Self::Matched),
            "Unmatched" => Ok(Self::Unmatched),
            other => Err(anyhow::anyhow!("unknown charge match status: {other}")),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ReceiptKind {
    NewSubscription,
    Renewal,
    OneTimePurchase,
    Refund,
}

impl ReceiptKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::NewSubscription => "new_subscription",
            Self::Renewal => "renewal",
            Self::OneTimePurchase => "one_time_purchase",
            Self::Refund => "refund",
        }
    }
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> anyhow::Result<Self> {
        match s {
            "new_subscription" => Ok(Self::NewSubscription),
            "renewal" => Ok(Self::Renewal),
            "one_time_purchase" => Ok(Self::OneTimePurchase),
            "refund" => Ok(Self::Refund),
            other => Err(anyhow::anyhow!("unknown receipt kind: {other}")),
        }
    }
}

#[derive(Debug, Clone)]
pub struct SubscriptionCharge {
    pub id: Uuid,
    pub subscription_id: Uuid,
    pub user_id: Uuid,
    pub amount: Decimal,
    pub currency: String,
    pub charged_at: DateTime<Utc>,
    pub email_message_id: String,
    pub kind: ReceiptKind,
    pub transaction_id: Option<Uuid>,
    pub match_status: ChargeMatchStatus,
    pub created_at: DateTime<Utc>,
}

#[async_trait::async_trait]
pub trait SubscriptionChargeRepository: Send + Sync {
    async fn create_idempotent(
        &self,
        charge: &SubscriptionCharge,
    ) -> anyhow::Result<(Uuid, bool)>;
    async fn find_by_id(
        &self,
        id: Uuid,
        user_id: Uuid,
    ) -> anyhow::Result<Option<SubscriptionCharge>>;
    async fn list_pending_for_user(&self, user_id: Uuid)
        -> anyhow::Result<Vec<SubscriptionCharge>>;
    async fn list_for_subscription(
        &self,
        subscription_id: Uuid,
        user_id: Uuid,
    ) -> anyhow::Result<Vec<SubscriptionCharge>>;
    async fn update_match(
        &self,
        id: Uuid,
        transaction_id: Option<Uuid>,
        match_status: ChargeMatchStatus,
    ) -> anyhow::Result<()>;
    async fn mark_pending_older_than_unmatched(
        &self,
        threshold: DateTime<Utc>,
    ) -> anyhow::Result<u64>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn match_status_roundtrip() {
        for s in [
            ChargeMatchStatus::Pending,
            ChargeMatchStatus::Matched,
            ChargeMatchStatus::Unmatched,
        ] {
            assert_eq!(ChargeMatchStatus::from_str(s.as_str()).unwrap(), s);
        }
    }

    #[test]
    fn receipt_kind_roundtrip() {
        for k in [
            ReceiptKind::NewSubscription,
            ReceiptKind::Renewal,
            ReceiptKind::OneTimePurchase,
            ReceiptKind::Refund,
        ] {
            assert_eq!(ReceiptKind::from_str(k.as_str()).unwrap(), k);
        }
    }
}
