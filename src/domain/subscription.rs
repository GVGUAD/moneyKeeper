use chrono::{DateTime, Utc};
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
    pub fn cycle_days(&self) -> i64 {
        match self {
            Self::Weekly => 7,
            Self::Monthly => 30,
            Self::Yearly => 365,
        }
    }
    pub fn next_after(&self, from: DateTime<Utc>) -> DateTime<Utc> {
        from + chrono::Duration::days(self.cycle_days())
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
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Default)]
pub struct SubscriptionListFilter {
    pub status: Option<SubscriptionStatus>,
}

#[async_trait::async_trait]
pub trait SubscriptionRepository: Send + Sync {
    async fn upsert_by_merchant_key(&self, sub: &Subscription) -> anyhow::Result<Subscription>;
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
        product_name: Option<String>,
        category_id: Option<Option<Uuid>>,
        billing_period: Option<BillingPeriod>,
        status: Option<SubscriptionStatus>,
    ) -> anyhow::Result<()>;
    async fn list_lapsed(&self, before: DateTime<Utc>) -> anyhow::Result<Vec<Subscription>>;
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
    fn billing_period_cycle_days() {
        assert_eq!(BillingPeriod::Weekly.cycle_days(), 7);
        assert_eq!(BillingPeriod::Monthly.cycle_days(), 30);
        assert_eq!(BillingPeriod::Yearly.cycle_days(), 365);
    }

    #[test]
    fn billing_period_next_after_is_one_cycle_later() {
        let from = DateTime::<Utc>::from_timestamp(1_700_000_000, 0).unwrap();
        let next = BillingPeriod::Monthly.next_after(from);
        assert_eq!((next - from).num_days(), 30);
    }
}
