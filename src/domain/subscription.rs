use chrono::{DateTime, Months, Utc};
use rust_decimal::Decimal;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq)]
pub enum SubscriptionProvider {
    GooglePlay,
    AppleAppStore,
    Netflix,
    Other,
}

impl SubscriptionProvider {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::GooglePlay => "google_play",
            Self::AppleAppStore => "apple_app_store",
            Self::Netflix => "netflix",
            Self::Other => "other",
        }
    }
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> anyhow::Result<Self> {
        match s {
            "google_play" => Ok(Self::GooglePlay),
            "apple_app_store" => Ok(Self::AppleAppStore),
            "netflix" => Ok(Self::Netflix),
            "other" => Ok(Self::Other),
            other => Err(anyhow::anyhow!("unknown subscription provider: {other}")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BillingPeriod {
    Weekly,
    Monthly,
    Yearly,
}

impl BillingPeriod {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Weekly => "weekly",
            Self::Monthly => "monthly",
            Self::Yearly => "yearly",
        }
    }
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> anyhow::Result<Self> {
        match s {
            "weekly" => Ok(Self::Weekly),
            "monthly" => Ok(Self::Monthly),
            "yearly" => Ok(Self::Yearly),
            other => Err(anyhow::anyhow!("unknown billing period: {other}")),
        }
    }
    pub fn next_after(&self, from: DateTime<Utc>) -> DateTime<Utc> {
        match self {
            Self::Weekly => from + chrono::Duration::weeks(1),
            Self::Monthly => from
                .checked_add_months(Months::new(1))
                .expect("monthly subscription date remains representable"),
            Self::Yearly => from
                .checked_add_months(Months::new(12))
                .expect("yearly subscription date remains representable"),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum SubscriptionStatus {
    Active,
    Inactive,
}

impl SubscriptionStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Inactive => "inactive",
        }
    }
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> anyhow::Result<Self> {
        match s {
            "active" => Ok(Self::Active),
            "inactive" => Ok(Self::Inactive),
            other => Err(anyhow::anyhow!("unknown subscription status: {other}")),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Subscription {
    pub id: Uuid,
    pub user_id: Uuid,
    pub provider: SubscriptionProvider,
    pub product_name: String,
    pub merchant_key: String,
    pub amount: Decimal,
    pub currency: String,
    pub billing_period: BillingPeriod,
    pub status: SubscriptionStatus,
    pub started_at: DateTime<Utc>,
    pub last_charged_at: Option<DateTime<Utc>>,
    pub next_expected_at: Option<DateTime<Utc>>,
    pub category_id: Option<Uuid>,
    pub overrides: SubscriptionOverrides,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Default)]
pub struct SubscriptionOverrides {
    pub product_name: Option<String>,
    pub billing_period: Option<BillingPeriod>,
    pub status: Option<SubscriptionStatus>,
}

#[derive(Debug, Clone, Default)]
pub struct SubscriptionListFilter {
    pub status: Option<SubscriptionStatus>,
}

#[derive(Debug, Clone)]
pub struct SubscriptionUpsertResult {
    pub subscription: Subscription,
    /// True only when this call inserted the aggregate. This is determined
    /// while holding the same merchant lock as the upsert, so receipt kind can
    /// be selected without a check-then-insert race.
    pub inserted: bool,
}

#[derive(Debug, Clone)]
pub enum TransactionSubscriptionTarget {
    Create {
        subscription_id: Uuid,
        product_name: String,
        billing_period: BillingPeriod,
    },
    Attach {
        subscription_id: Uuid,
    },
}

#[derive(Debug, Clone)]
pub struct MarkTransactionSubscription {
    pub user_id: Uuid,
    pub transaction_id: Uuid,
    pub target: TransactionSubscriptionTarget,
    pub requested_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkTransactionSubscriptionOutcome {
    Created {
        subscription_id: Uuid,
        charge_id: Uuid,
        subscription_created: bool,
    },
    AlreadyLinked {
        subscription_id: Uuid,
        charge_id: Uuid,
    },
    TransactionNotFound,
    TransactionNotExpense,
    TransactionInvalid,
    SubscriptionNotFound,
    TransactionAlreadyLinked {
        subscription_id: Uuid,
        charge_id: Uuid,
    },
}

#[async_trait::async_trait]
pub trait SubscriptionRepository: Send + Sync {
    /// Atomically creates or attaches a subscription charge for an existing
    /// expense transaction. Implementations must reserve the transaction so a
    /// concurrent request cannot create a second charge link.
    async fn mark_transaction_as_subscription(
        &self,
        command: &MarkTransactionSubscription,
    ) -> anyhow::Result<MarkTransactionSubscriptionOutcome>;
    async fn upsert_by_merchant_key(&self, sub: &Subscription) -> anyhow::Result<Subscription>;
    /// Atomically upserts receipt-derived state unless the user previously
    /// deleted this provider/merchant. `None` means a tombstone suppressed it.
    async fn upsert_receipt_if_not_tombstoned(
        &self,
        sub: &Subscription,
    ) -> anyhow::Result<Option<SubscriptionUpsertResult>>;
    async fn find_by_id(&self, id: Uuid, user_id: Uuid) -> anyhow::Result<Option<Subscription>>;
    async fn list_by_user(
        &self,
        user_id: Uuid,
        filter: &SubscriptionListFilter,
    ) -> anyhow::Result<Vec<Subscription>>;
    async fn update_after_charge(
        &self,
        id: Uuid,
        last_charged_at: DateTime<Utc>,
        next_expected_at: DateTime<Utc>,
        status: SubscriptionStatus,
    ) -> anyhow::Result<()>;
    async fn update_editable_fields(
        &self,
        id: Uuid,
        user_id: Uuid,
        product_name: Option<Option<String>>,
        category_id: Option<Option<Uuid>>,
        billing_period: Option<Option<BillingPeriod>>,
        status: Option<Option<SubscriptionStatus>>,
    ) -> anyhow::Result<()>;
    async fn list_lapsed(&self, before: DateTime<Utc>) -> anyhow::Result<Vec<Subscription>>;
    /// Atomically updates only automatic lifecycle state that is still due at
    /// statement execution time. Manual status/period overrides and newer
    /// receipts cannot be overwritten by a stale detector read.
    async fn mark_lapsed(&self, before: DateTime<Utc>) -> anyhow::Result<u64>;
    async fn delete(&self, id: Uuid, user_id: Uuid) -> anyhow::Result<()>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_roundtrip() {
        for p in [
            SubscriptionProvider::GooglePlay,
            SubscriptionProvider::AppleAppStore,
            SubscriptionProvider::Netflix,
            SubscriptionProvider::Other,
        ] {
            assert_eq!(SubscriptionProvider::from_str(p.as_str()).unwrap(), p);
        }
    }

    #[test]
    fn billing_period_next_after_uses_calendar_months_with_month_end_clamping() {
        let january_31 = "2024-01-31T10:15:00Z".parse::<DateTime<Utc>>().unwrap();
        assert_eq!(
            BillingPeriod::Monthly.next_after(january_31),
            "2024-02-29T10:15:00Z".parse::<DateTime<Utc>>().unwrap()
        );
    }

    #[test]
    fn yearly_recurrence_clamps_leap_day() {
        let leap_day = "2024-02-29T00:00:00Z".parse::<DateTime<Utc>>().unwrap();
        assert_eq!(
            BillingPeriod::Yearly.next_after(leap_day),
            "2025-02-28T00:00:00Z".parse::<DateTime<Utc>>().unwrap()
        );
    }
}
