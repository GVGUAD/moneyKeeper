use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use uuid::Uuid;

use crate::domain::email::EmailFetcher;
#[cfg(test)]
use crate::domain::email::RawEmail;
use crate::domain::email_connection::{
    EmailConnection, EmailConnectionRepository, EmailConnectionStatus, EmailProvider,
};
use crate::domain::fx_rate::FxRateRepository;
use crate::domain::receipt_parser::ParsedReceipt;
use crate::domain::subscription::{
    BillingPeriod, Subscription, SubscriptionListFilter, SubscriptionRepository, SubscriptionStatus,
};
use crate::domain::subscription_charge::{
    ChargeMatchStatus, SubscriptionCharge, SubscriptionChargeRepository,
};
use crate::domain::subscription_error::SubscriptionError;
use crate::infrastructure::email::parsers::ParserRegistry;

pub struct Forecast {
    pub base_currency: String,
    pub base_total: Decimal,
    pub by_currency: HashMap<String, Decimal>,
}

pub struct ConnectGmailParams {
    pub email_address: String,
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: DateTime<Utc>,
}

pub struct SubscriptionService {
    pub connections: Arc<dyn EmailConnectionRepository>,
    pub subscriptions: Arc<dyn SubscriptionRepository>,
    pub charges: Arc<dyn SubscriptionChargeRepository>,
    pub fetcher: Arc<dyn EmailFetcher>,
    pub parsers: Arc<ParserRegistry>,
}

impl SubscriptionService {
    pub fn new(
        connections: Arc<dyn EmailConnectionRepository>,
        subscriptions: Arc<dyn SubscriptionRepository>,
        charges: Arc<dyn SubscriptionChargeRepository>,
        fetcher: Arc<dyn EmailFetcher>,
        parsers: Arc<ParserRegistry>,
    ) -> Self {
        Self {
            connections,
            subscriptions,
            charges,
            fetcher,
            parsers,
        }
    }

    pub async fn connect_gmail(
        &self,
        user_id: Uuid,
        params: ConnectGmailParams,
    ) -> anyhow::Result<EmailConnection> {
        let conn = EmailConnection {
            id: Uuid::new_v4(),
            user_id,
            provider: EmailProvider::Gmail,
            email_address: params.email_address,
            oauth_access_token: params.access_token,
            oauth_refresh_token: params.refresh_token,
            access_token_expires_at: params.expires_at,
            status: EmailConnectionStatus::Connected,
            last_synced_at: None,
            last_history_id: None,
            created_at: Utc::now(),
        };
        self.connections.create(&conn).await?;
        Ok(conn)
    }

    pub async fn list_connections(&self, user_id: Uuid) -> anyhow::Result<Vec<EmailConnection>> {
        self.connections.list_by_user(user_id).await
    }

    pub async fn delete_connection(&self, id: Uuid, user_id: Uuid) -> anyhow::Result<()> {
        let exists = self.connections.find_by_id(id, user_id).await?.is_some();
        if !exists {
            return Err(SubscriptionError::ConnectionNotFound.into());
        }
        self.connections.delete(id, user_id).await
    }

    /// Sync one connection. Returns newly-inserted charge ids.
    pub async fn sync_connection(&self, conn_id: Uuid) -> anyhow::Result<Vec<Uuid>> {
        let conn = self
            .connections
            .list_connected()
            .await?
            .into_iter()
            .find(|c| c.id == conn_id)
            .ok_or(SubscriptionError::ConnectionNotFound)?;

        let (emails, new_cursor) = self.fetcher.fetch_new(&conn).await?;
        let mut new_charge_ids = Vec::new();
        for email in emails {
            let Some(parser) = self.parsers.find(&email.from) else {
                continue;
            };
            let Some(receipt) = parser.parse(&email)? else {
                continue;
            };
            let sub = self
                .upsert_subscription_from_receipt(conn.user_id, &receipt)
                .await?;
            let charge = SubscriptionCharge {
                id: Uuid::new_v4(),
                subscription_id: sub.id,
                user_id: conn.user_id,
                amount: receipt.amount,
                currency: receipt.currency.clone(),
                charged_at: receipt.charged_at,
                email_message_id: email.message_id.clone(),
                kind: receipt.kind.clone(),
                transaction_id: None,
                match_status: ChargeMatchStatus::Pending,
                created_at: Utc::now(),
            };
            let (id, inserted) = self.charges.create_idempotent(&charge).await?;
            if inserted {
                new_charge_ids.push(id);
            }
        }
        self.connections
            .update_sync_cursor(conn.id, Utc::now(), new_cursor)
            .await?;
        Ok(new_charge_ids)
    }

    pub async fn list(
        &self,
        user_id: Uuid,
        status: Option<SubscriptionStatus>,
    ) -> anyhow::Result<Vec<Subscription>> {
        self.subscriptions
            .list_by_user(user_id, &SubscriptionListFilter { status })
            .await
    }

    pub async fn forecast_next_30d(
        &self,
        user_id: Uuid,
        base_currency: &str,
        fx: &dyn FxRateRepository,
    ) -> anyhow::Result<Forecast> {
        let subs = self
            .subscriptions
            .list_by_user(
                user_id,
                &SubscriptionListFilter {
                    status: Some(SubscriptionStatus::Active),
                },
            )
            .await?;
        let mut by_currency: HashMap<String, Decimal> = HashMap::new();
        let mut base_total = Decimal::ZERO;
        let today = Utc::now().date_naive();
        let thirty = Decimal::from(30);
        for s in subs {
            let cycle_days = Decimal::from(s.billing_period.cycle_days());
            let monthly_equivalent = s.amount * thirty / cycle_days;
            *by_currency
                .entry(s.currency.clone())
                .or_insert(Decimal::ZERO) += monthly_equivalent;
            let in_base = if s.currency == base_currency {
                monthly_equivalent
            } else {
                let rate = fx
                    .rate_as_of(today, &s.currency, base_currency)
                    .await?
                    .ok_or_else(|| {
                        anyhow::anyhow!("no FX rate {} → {}", s.currency, base_currency)
                    })?;
                monthly_equivalent * rate
            };
            base_total += in_base;
        }
        Ok(Forecast {
            base_currency: base_currency.to_string(),
            base_total,
            by_currency,
        })
    }

    async fn upsert_subscription_from_receipt(
        &self,
        user_id: Uuid,
        receipt: &ParsedReceipt,
    ) -> anyhow::Result<Subscription> {
        let billing_period = receipt
            .billing_period_hint
            .unwrap_or(BillingPeriod::Monthly);
        let now = Utc::now();
        let sub = Subscription {
            id: Uuid::new_v4(),
            user_id,
            provider: receipt.provider.clone(),
            product_name: receipt.product_name.clone(),
            merchant_key: receipt.merchant_key.clone(),
            amount: receipt.amount,
            currency: receipt.currency.clone(),
            billing_period,
            status: SubscriptionStatus::Active,
            started_at: receipt.charged_at,
            last_charged_at: None,
            next_expected_at: None,
            category_id: None,
            created_at: now,
        };
        self.subscriptions.upsert_by_merchant_key(&sub).await
    }
}

#[cfg(test)]
mod test_support {
    use super::*;

    pub struct FakeFetcher {
        pub emails: Vec<RawEmail>,
    }

    #[async_trait::async_trait]
    impl EmailFetcher for FakeFetcher {
        async fn fetch_new(
            &self,
            _conn: &EmailConnection,
        ) -> anyhow::Result<(Vec<RawEmail>, Option<String>)> {
            Ok((self.emails.clone(), Some("cursor-1".to_string())))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::FakeFetcher;
    use super::*;
    use crate::domain::subscription::SubscriptionRepository;
    use crate::domain::subscription_charge::SubscriptionChargeRepository;
    use crate::infrastructure::email_connection_repository::PgEmailConnectionRepository;
    use crate::infrastructure::subscription_charge_repository::PgSubscriptionChargeRepository;
    use crate::infrastructure::subscription_repository::PgSubscriptionRepository;
    use crate::infrastructure::test_db;

    fn make_svc(pool: sqlx::PgPool) -> SubscriptionService {
        let conns: Arc<dyn EmailConnectionRepository> =
            Arc::new(PgEmailConnectionRepository::new(pool.clone()));
        let subs: Arc<dyn SubscriptionRepository> =
            Arc::new(PgSubscriptionRepository::new(pool.clone()));
        let charges: Arc<dyn SubscriptionChargeRepository> =
            Arc::new(PgSubscriptionChargeRepository::new(pool.clone()));
        SubscriptionService::new(
            conns,
            subs,
            charges,
            Arc::new(FakeFetcher { emails: vec![] }),
            Arc::new(ParserRegistry::default_set()),
        )
    }

    #[tokio::test]
    async fn connect_gmail_persists_connected_status() {
        let pool = test_db::fresh_pool().await;
        let repo: Arc<dyn EmailConnectionRepository> =
            Arc::new(PgEmailConnectionRepository::new(pool.clone()));
        let svc = make_svc(pool);
        let user_id = Uuid::new_v4();
        let conn = svc
            .connect_gmail(
                user_id,
                ConnectGmailParams {
                    email_address: "x@y.com".into(),
                    access_token: "a".into(),
                    refresh_token: "r".into(),
                    expires_at: Utc::now() + chrono::Duration::hours(1),
                },
            )
            .await
            .unwrap();
        assert_eq!(conn.status, EmailConnectionStatus::Connected);
        let found = repo.find_by_id(conn.id, user_id).await.unwrap().unwrap();
        assert_eq!(found.email_address, "x@y.com");
    }

    #[tokio::test]
    async fn delete_returns_error_when_missing() {
        let pool = test_db::fresh_pool().await;
        let svc = make_svc(pool);
        let err = svc
            .delete_connection(Uuid::new_v4(), Uuid::new_v4())
            .await
            .unwrap_err();
        assert!(err.downcast_ref::<SubscriptionError>().is_some());
    }
}

#[cfg(test)]
mod sync_tests {
    use super::test_support::FakeFetcher;
    use super::*;
    use crate::domain::email_connection::EmailConnectionRepository;
    use crate::domain::subscription::SubscriptionRepository;
    use crate::domain::subscription_charge::SubscriptionChargeRepository;
    use crate::infrastructure::email_connection_repository::PgEmailConnectionRepository;
    use crate::infrastructure::subscription_charge_repository::PgSubscriptionChargeRepository;
    use crate::infrastructure::subscription_repository::PgSubscriptionRepository;
    use crate::infrastructure::test_db;

    fn netflix_email(msg_id: &str) -> RawEmail {
        RawEmail {
            message_id: msg_id.to_string(),
            from: "Netflix <info@account.netflix.com>".into(),
            subject: "Your Netflix payment".into(),
            received_at: Utc::now(),
            body_text: Some("Plan: Netflix Premium\nTotal: $15.99 USD\nDate: May 18, 2026".into()),
            body_html: None,
        }
    }

    #[tokio::test]
    async fn sync_creates_subscription_and_charge_then_is_idempotent() {
        let pool = test_db::fresh_pool().await;
        let user_id = Uuid::new_v4();
        let conns: Arc<dyn EmailConnectionRepository> =
            Arc::new(PgEmailConnectionRepository::new(pool.clone()));
        let subs: Arc<dyn SubscriptionRepository> =
            Arc::new(PgSubscriptionRepository::new(pool.clone()));
        let charges: Arc<dyn SubscriptionChargeRepository> =
            Arc::new(PgSubscriptionChargeRepository::new(pool.clone()));
        let svc = SubscriptionService::new(
            conns.clone(),
            subs,
            charges.clone(),
            Arc::new(FakeFetcher {
                emails: vec![netflix_email("<m1>")],
            }),
            Arc::new(ParserRegistry::default_set()),
        );

        let conn = svc
            .connect_gmail(
                user_id,
                ConnectGmailParams {
                    email_address: "x@y.com".into(),
                    access_token: "a".into(),
                    refresh_token: "r".into(),
                    expires_at: Utc::now() + chrono::Duration::hours(1),
                },
            )
            .await
            .unwrap();

        let ids1 = svc.sync_connection(conn.id).await.unwrap();
        assert_eq!(ids1.len(), 1);

        // Second sync with same email → 0 new ids (idempotent on email_message_id).
        let ids2 = svc.sync_connection(conn.id).await.unwrap();
        assert!(ids2.is_empty());

        let pending = charges.list_pending_for_user(user_id).await.unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].amount.to_string(), "15.99");
    }
}

#[cfg(test)]
mod forecast_tests {
    use std::sync::Arc;

    use chrono::Utc;
    use rust_decimal_macros::dec;
    use uuid::Uuid;

    use super::test_support::FakeFetcher;
    use super::*;
    use crate::domain::email_connection::EmailConnectionRepository;
    use crate::domain::fx_rate::FxRateRepository;
    use crate::domain::subscription::{
        BillingPeriod, Subscription, SubscriptionProvider, SubscriptionRepository,
        SubscriptionStatus,
    };
    use crate::domain::subscription_charge::SubscriptionChargeRepository;
    use crate::infrastructure::email_connection_repository::PgEmailConnectionRepository;
    use crate::infrastructure::fx_rate_repository::PgFxRateRepository;
    use crate::infrastructure::subscription_charge_repository::PgSubscriptionChargeRepository;
    use crate::infrastructure::subscription_repository::PgSubscriptionRepository;
    use crate::infrastructure::test_db;

    #[tokio::test]
    async fn forecast_sums_active_subs_normalized_to_monthly() {
        let pool = test_db::fresh_pool().await;
        let user_id = Uuid::new_v4();
        let conns: Arc<dyn EmailConnectionRepository> =
            Arc::new(PgEmailConnectionRepository::new(pool.clone()));
        let subs: Arc<dyn SubscriptionRepository> =
            Arc::new(PgSubscriptionRepository::new(pool.clone()));
        let charges: Arc<dyn SubscriptionChargeRepository> =
            Arc::new(PgSubscriptionChargeRepository::new(pool.clone()));
        let fx: Arc<dyn FxRateRepository> = Arc::new(PgFxRateRepository::new(pool.clone()));
        let svc = SubscriptionService::new(
            conns,
            subs.clone(),
            charges,
            Arc::new(FakeFetcher { emails: vec![] }),
            Arc::new(ParserRegistry::default_set()),
        );

        subs.upsert_by_merchant_key(&Subscription {
            id: Uuid::new_v4(),
            user_id,
            provider: SubscriptionProvider::Netflix,
            product_name: "Netflix".into(),
            merchant_key: "netflix.com:premium".into(),
            amount: dec!(15.99),
            currency: "USD".into(),
            billing_period: BillingPeriod::Monthly,
            status: SubscriptionStatus::Active,
            started_at: Utc::now(),
            last_charged_at: None,
            next_expected_at: None,
            category_id: None,
            created_at: Utc::now(),
        })
        .await
        .unwrap();

        subs.upsert_by_merchant_key(&Subscription {
            id: Uuid::new_v4(),
            user_id,
            provider: SubscriptionProvider::AppleAppStore,
            product_name: "iCloud+".into(),
            merchant_key: "apps.apple.com:icloud_50gb".into(),
            amount: dec!(12.00),
            currency: "USD".into(),
            billing_period: BillingPeriod::Yearly,
            status: SubscriptionStatus::Active,
            started_at: Utc::now(),
            last_charged_at: None,
            next_expected_at: None,
            category_id: None,
            created_at: Utc::now(),
        })
        .await
        .unwrap();

        let f = svc.forecast_next_30d(user_id, "USD", &*fx).await.unwrap();
        // 15.99 + 12 * 30 / 365 ≈ 15.99 + 0.986 = ~16.98
        assert!(f.base_total > dec!(16.90));
        assert!(f.base_total < dec!(17.10));
    }
}
