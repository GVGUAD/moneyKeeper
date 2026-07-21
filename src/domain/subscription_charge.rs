use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChargeSource {
    Gmail,
    Manual,
    Other,
}

impl ChargeSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Gmail => "gmail",
            Self::Manual => "manual",
            Self::Other => "other",
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> anyhow::Result<Self> {
        match s {
            "gmail" => Ok(Self::Gmail),
            "manual" => Ok(Self::Manual),
            "other" => Ok(Self::Other),
            other => Err(anyhow::anyhow!(
                "unknown subscription charge source: {other}"
            )),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChargeMatchSource {
    Automatic,
    Manual,
}

impl ChargeMatchSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Automatic => "automatic",
            Self::Manual => "manual",
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> anyhow::Result<Self> {
        match s {
            "automatic" => Ok(Self::Automatic),
            "manual" => Ok(Self::Manual),
            other => Err(anyhow::anyhow!("unknown charge match source: {other}")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChargeLinkOutcome {
    Linked,
    ChargeNotFound,
    ChargeNotPending,
    ChargeAlreadyLinked,
    TransactionNotFound,
    TransactionNotExpense,
    TransactionAlreadyLinked,
}

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
    /// Optional RFC 822 Message-ID. This is retained only to attach the first
    /// post-migration Gmail replay to a legacy charge row.
    pub rfc_message_id: Option<String>,
    pub source: ChargeSource,
    /// Stable, globally unique identifier inside `source` (for Gmail this is
    /// `gmail:{connection_id}:{provider_message_id}`).
    pub source_key: String,
    pub source_connection_id: Option<Uuid>,
    /// Gmail's provider-assigned message id. Legacy rows remain `None` until
    /// their first replay attaches a mailbox-scoped source identity.
    pub provider_message_id: Option<String>,
    pub kind: ReceiptKind,
    pub transaction_id: Option<Uuid>,
    pub match_status: ChargeMatchStatus,
    pub match_started_at: DateTime<Utc>,
    pub match_source: Option<ChargeMatchSource>,
    pub created_at: DateTime<Utc>,
}

#[async_trait::async_trait]
pub trait SubscriptionChargeRepository: Send + Sync {
    async fn create_idempotent(&self, charge: &SubscriptionCharge) -> anyhow::Result<(Uuid, bool)>;
    async fn find_by_id(
        &self,
        id: Uuid,
        user_id: Uuid,
    ) -> anyhow::Result<Option<SubscriptionCharge>>;
    async fn list_pending_for_user(&self, user_id: Uuid)
    -> anyhow::Result<Vec<SubscriptionCharge>>;
    async fn list_users_with_pending(&self) -> anyhow::Result<Vec<Uuid>>;
    async fn list_for_subscription(
        &self,
        subscription_id: Uuid,
        user_id: Uuid,
    ) -> anyhow::Result<Vec<SubscriptionCharge>>;
    /// Atomically reserves `transaction_id`, links the charge, propagates a
    /// same-user subscription category, and leaves receipt-derived lifecycle
    /// state unchanged. The database unique index is the final concurrency guard.
    async fn link_transaction(
        &self,
        id: Uuid,
        user_id: Uuid,
        transaction_id: Uuid,
        source: ChargeMatchSource,
    ) -> anyhow::Result<ChargeLinkOutcome>;
    /// Atomically unlinks a charge. When `reject_transaction` is true, the old
    /// transaction is recorded so automatic matching cannot immediately relink it.
    async fn unlink_transaction(
        &self,
        id: Uuid,
        user_id: Uuid,
        reject_transaction: bool,
    ) -> anyhow::Result<bool>;
    async fn mark_pending_older_than_unmatched(
        &self,
        user_id: Uuid,
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

    #[test]
    fn charge_source_roundtrip() {
        for source in [
            ChargeSource::Gmail,
            ChargeSource::Manual,
            ChargeSource::Other,
        ] {
            assert_eq!(ChargeSource::from_str(source.as_str()).unwrap(), source);
        }
    }

    #[test]
    fn match_source_roundtrip() {
        for source in [ChargeMatchSource::Automatic, ChargeMatchSource::Manual] {
            assert_eq!(
                ChargeMatchSource::from_str(source.as_str()).unwrap(),
                source
            );
        }
    }
}
