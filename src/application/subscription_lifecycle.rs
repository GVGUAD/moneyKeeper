use std::sync::Arc;

use chrono::{Duration, Utc};

use crate::domain::subscription::SubscriptionRepository;

pub struct DetectLapsedUseCase {
    pub subscriptions: Arc<dyn SubscriptionRepository>,
}

impl DetectLapsedUseCase {
    pub async fn run(&self) -> anyhow::Result<usize> {
        let threshold = Utc::now() - Duration::days(7);
        Ok(self.subscriptions.mark_lapsed(threshold).await? as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::subscription::{
        BillingPeriod, Subscription, SubscriptionProvider, SubscriptionStatus,
    };
    use crate::infrastructure::subscription_repository::PgSubscriptionRepository;
    use crate::infrastructure::test_db;
    use rust_decimal_macros::dec;
    use uuid::Uuid;

    #[tokio::test]
    async fn marks_subs_past_threshold_inactive() {
        let pool = test_db::fresh_pool().await;
        let repo: Arc<dyn SubscriptionRepository> = Arc::new(PgSubscriptionRepository::new(pool));
        let user_id = Uuid::new_v4();
        let s = Subscription {
            id: Uuid::new_v4(),
            user_id,
            provider: SubscriptionProvider::Netflix,
            product_name: "Netflix".into(),
            merchant_key: "netflix.com:premium".into(),
            amount: dec!(15.99),
            currency: "USD".into(),
            billing_period: BillingPeriod::Monthly,
            status: SubscriptionStatus::Active,
            started_at: Utc::now() - Duration::days(60),
            last_charged_at: Some(Utc::now() - Duration::days(40)),
            next_expected_at: Some(Utc::now() - Duration::days(10)),
            category_id: None,
            overrides: Default::default(),
            created_at: Utc::now(),
        };
        repo.upsert_by_merchant_key(&s).await.unwrap();

        let uc = DetectLapsedUseCase {
            subscriptions: repo.clone(),
        };
        let n = uc.run().await.unwrap();
        assert_eq!(n, 1);

        let updated = repo.find_by_id(s.id, user_id).await.unwrap().unwrap();
        assert_eq!(updated.status, SubscriptionStatus::Inactive);
    }
}
