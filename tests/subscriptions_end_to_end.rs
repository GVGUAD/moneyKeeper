mod common;

use std::sync::Arc;
use std::time::Duration as StdDuration;

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use moneykeeper::application::monobank::MonobankService;
use moneykeeper::application::subscription_matching::MatchChargesUseCase;
use moneykeeper::application::subscriptions::SubscriptionService;
use moneykeeper::domain::account::{Account, AccountDetails, AccountRepository, AccountType};
use moneykeeper::domain::bank_connection::{
    BankConnection, BankConnectionRepository, BankProvider,
};
use moneykeeper::domain::category::{Category, CategoryRepository};
use moneykeeper::domain::email::{EmailFetchBatch, EmailFetcher, RawEmail};
use moneykeeper::domain::email_connection::{EmailConnection, EmailConnectionRepository};
use moneykeeper::domain::email_sync::EmailSyncRepository;
use moneykeeper::domain::fx_rate::{FxRate, FxRateRepository};
use moneykeeper::domain::monobank::{MonoAccount, MonoStatementItem, MonobankApiClient};
use moneykeeper::domain::subscription::{SubscriptionListFilter, SubscriptionRepository};
use moneykeeper::domain::subscription_charge::{
    ChargeMatchSource, ChargeMatchStatus, SubscriptionCharge, SubscriptionChargeRepository,
};
use moneykeeper::domain::transaction::TransactionRepository;
use moneykeeper::infrastructure::account_repository::SqliteAccountRepository;
use moneykeeper::infrastructure::category_repository::SqliteCategoryRepository;
use moneykeeper::infrastructure::credential_crypto::{SecretValue, TokenCipher, TokenCipherConfig};
use moneykeeper::infrastructure::email::oauth::{
    GmailOAuthClient, GmailOAuthService, GmailProfile, GmailTokenSet, OAuthConfig, OAuthFlowError,
    PgGmailOAuthStateStore,
};
use moneykeeper::infrastructure::email::parsers::ParserRegistry;
use moneykeeper::infrastructure::email_connection_repository::PgEmailConnectionRepository;
use moneykeeper::infrastructure::email_sync_repository::PgEmailSyncRepository;
use moneykeeper::infrastructure::fx_rate_repository::PgFxRateRepository;
use moneykeeper::infrastructure::monobank_repository::PgBankConnectionRepository;
use moneykeeper::infrastructure::subscription_charge_repository::PgSubscriptionChargeRepository;
use moneykeeper::infrastructure::subscription_repository::PgSubscriptionRepository;
use moneykeeper::infrastructure::transaction_repository::SqliteTransactionRepository;
use rust_decimal_macros::dec;
use uuid::Uuid;

struct FakeGmailOAuthClient;

#[async_trait]
impl GmailOAuthClient for FakeGmailOAuthClient {
    fn authorization_url(&self, state: &str, pkce_challenge: &str) -> anyhow::Result<String> {
        Ok(format!(
            "https://gmail.test/authorize?state={state}&code_challenge={pkce_challenge}"
        ))
    }

    async fn exchange_code(
        &self,
        code: &str,
        pkce_verifier: &str,
    ) -> anyhow::Result<GmailTokenSet> {
        anyhow::ensure!(code == "one-time-code", "unexpected OAuth code");
        anyhow::ensure!(pkce_verifier.len() >= 43, "PKCE verifier was not restored");
        Ok(GmailTokenSet {
            access_token: SecretValue::new("gmail-access-token"),
            refresh_token: Some(SecretValue::new("gmail-refresh-token")),
            // Force the first sync through the refresh path so the end-to-end
            // test proves refreshed credentials are persisted before Gmail is
            // called.
            expires_at: Utc::now() - Duration::minutes(1),
        })
    }

    async fn profile(&self, access_token: &str) -> anyhow::Result<GmailProfile> {
        anyhow::ensure!(
            access_token == "gmail-access-token",
            "unexpected access token"
        );
        Ok(GmailProfile {
            email_address: " Subscriber@Example.COM ".to_string(),
            verified: true,
        })
    }

    async fn refresh(&self, _refresh_token: &str) -> anyhow::Result<GmailTokenSet> {
        Ok(GmailTokenSet {
            access_token: SecretValue::new("refreshed-access-token"),
            refresh_token: None,
            expires_at: Utc::now() + Duration::hours(1),
        })
    }

    async fn revoke(&self, _token: &str) -> anyhow::Result<()> {
        Ok(())
    }
}

struct FakeRecurringGmailFetcher {
    email: RawEmail,
}

#[async_trait]
impl EmailFetcher for FakeRecurringGmailFetcher {
    async fn fetch_new(&self, connection: &EmailConnection) -> anyhow::Result<EmailFetchBatch> {
        anyhow::ensure!(
            connection.oauth_access_token == "refreshed-access-token",
            "Gmail fetch received stale OAuth credentials"
        );
        Ok(EmailFetchBatch {
            emails: vec![self.email.clone()],
            failures: vec![],
            ignored_message_ids: vec![],
            next_history_id: Some("history-e2e-1".to_string()),
            history_was_reset: false,
        })
    }
}

struct NoopMonobankClient;

#[async_trait]
impl MonobankApiClient for NoopMonobankClient {
    async fn get_accounts(&self, _token: &str) -> anyhow::Result<Vec<MonoAccount>> {
        Ok(vec![])
    }

    async fn get_statement(
        &self,
        _token: &str,
        _account_id: &str,
        _from: DateTime<Utc>,
        _to: DateTime<Utc>,
    ) -> anyhow::Result<Vec<MonoStatementItem>> {
        Ok(vec![])
    }

    async fn set_webhook(&self, _token: &str, _webhook_url: &str) -> anyhow::Result<()> {
        Ok(())
    }
}

async fn wait_for_automatic_match(
    charges: &dyn SubscriptionChargeRepository,
    charge_id: Uuid,
    user_id: Uuid,
) -> anyhow::Result<SubscriptionCharge> {
    for _ in 0..100 {
        let charge = charges
            .find_by_id(charge_id, user_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("charge disappeared while waiting for matcher"))?;
        if charge.match_status == ChargeMatchStatus::Matched {
            return Ok(charge);
        }
        tokio::time::sleep(StdDuration::from_millis(50)).await;
    }
    anyhow::bail!("Monobank webhook matcher did not link the charge in time")
}

#[tokio::test]
async fn oauth_gmail_ledger_fx_monobank_matching_is_end_to_end_idempotent() -> anyhow::Result<()> {
    let postgres = common::TestPostgres::new().await;
    let pool = postgres.pool.clone();
    let user_id = Uuid::new_v4();
    let charged_at = "2026-07-10T00:00:00Z".parse::<DateTime<Utc>>()?;

    let cipher = Arc::new(TokenCipher::new(TokenCipherConfig {
        active_key_id: "subscriptions-e2e".to_string(),
        keys: [("subscriptions-e2e".to_string(), [73_u8; 32])]
            .into_iter()
            .collect(),
    }));
    let email_connections: Arc<dyn EmailConnectionRepository> = Arc::new(
        PgEmailConnectionRepository::with_cipher(pool.clone(), Arc::clone(&cipher)),
    );
    let oauth_client: Arc<dyn GmailOAuthClient> = Arc::new(FakeGmailOAuthClient);
    let oauth_config = OAuthConfig::new(
        "gmail-client".to_string(),
        "gmail-secret",
        "https://api.example.test/oauth/gmail/callback".to_string(),
    );
    let oauth = GmailOAuthService::new(
        Arc::clone(&oauth_client),
        Arc::new(PgGmailOAuthStateStore::new(
            pool.clone(),
            Arc::clone(&cipher),
        )),
        Arc::clone(&email_connections),
        oauth_config,
    );

    let start = oauth.start(user_id).await?;
    assert!(start.authorize_url.contains(&start.state));
    assert!(start.authorize_url.contains("code_challenge="));
    let gmail_connection = oauth
        .complete("one-time-code", &start.state, Some(user_id))
        .await?;
    assert_eq!(gmail_connection.email_address, "subscriber@example.com");
    let replay_error = oauth
        .complete("one-time-code", &start.state, Some(user_id))
        .await
        .unwrap_err();
    assert!(matches!(
        replay_error.downcast_ref::<OAuthFlowError>(),
        Some(OAuthFlowError::InvalidState)
    ));

    let subscriptions: Arc<dyn SubscriptionRepository> =
        Arc::new(PgSubscriptionRepository::new(pool.clone()));
    let charges: Arc<dyn SubscriptionChargeRepository> =
        Arc::new(PgSubscriptionChargeRepository::new(pool.clone()));
    let email_sync: Arc<dyn EmailSyncRepository> =
        Arc::new(PgEmailSyncRepository::new(pool.clone()));
    let fetcher: Arc<dyn EmailFetcher> = Arc::new(FakeRecurringGmailFetcher {
        email: RawEmail {
            provider_message_id: "gmail-netflix-e2e-1".to_string(),
            rfc_message_id: Some("<netflix-e2e-1@example.test>".to_string()),
            from: "Netflix <info@account.netflix.com>".to_string(),
            subject: "Your Netflix payment".to_string(),
            authentication_results: vec![],
            received_at: charged_at + Duration::hours(1),
            body_text: Some(
                "Plan: Netflix Premium\nTotal: $15.99 USD\nDate: July 10, 2026".to_string(),
            ),
            body_html: None,
        },
    });
    let subscription_service = SubscriptionService::new(
        Arc::clone(&email_connections),
        Arc::clone(&subscriptions),
        Arc::clone(&charges),
        fetcher,
        Arc::new(ParserRegistry::default_set()),
    )
    .with_reliable_sync(Arc::clone(&email_sync), Arc::clone(&oauth_client));

    let first_sync = subscription_service
        .sync_connection_for_user(gmail_connection.id, user_id, false)
        .await?;
    assert_eq!(first_sync.len(), 1);
    let refreshed_connection = email_connections
        .find_by_id(gmail_connection.id, user_id)
        .await?
        .expect("Gmail connection remains available after refresh");
    assert_eq!(
        refreshed_connection.oauth_access_token,
        "refreshed-access-token"
    );
    assert_eq!(
        refreshed_connection.oauth_refresh_token, "gmail-refresh-token",
        "refresh responses without a replacement retain the stored token"
    );
    let charge_id = first_sync[0];
    let pending = charges
        .find_by_id(charge_id, user_id)
        .await?
        .expect("Gmail sync created a charge");
    assert_eq!(pending.match_status, ChargeMatchStatus::Pending);
    assert_eq!(pending.transaction_id, None);
    assert_eq!(pending.amount, dec!(15.99));
    assert_eq!(pending.currency, "USD");
    assert_eq!(
        pending.source_key,
        format!("gmail:{}:gmail-netflix-e2e-1", gmail_connection.id)
    );

    let user_subscriptions = subscriptions
        .list_by_user(user_id, &SubscriptionListFilter::default())
        .await?;
    assert_eq!(user_subscriptions.len(), 1);
    let subscription = &user_subscriptions[0];
    let category_repository = SqliteCategoryRepository::new(pool.clone());
    let category = Category::new(
        user_id,
        "Streaming".to_string(),
        Some("#6d28d9".to_string()),
    );
    category_repository.create(&category).await?;
    subscriptions
        .update_editable_fields(
            subscription.id,
            user_id,
            None,
            Some(Some(category.id)),
            None,
            None,
        )
        .await?;

    let fx: Arc<dyn FxRateRepository> = Arc::new(PgFxRateRepository::new(pool.clone()));
    fx.upsert_many(&[FxRate {
        rate_date: charged_at.date_naive(),
        from_currency: "USD".to_string(),
        to_currency: "UAH".to_string(),
        rate: dec!(40),
    }])
    .await?;

    let accounts: Arc<dyn AccountRepository> = Arc::new(SqliteAccountRepository::new(pool.clone()));
    let account = Account::new(
        user_id,
        "Monobank UAH".to_string(),
        AccountType::Bank,
        "UAH".to_string(),
    );
    accounts.create(&account, &AccountDetails::None).await?;
    let bank_connections: Arc<dyn BankConnectionRepository> = Arc::new(
        PgBankConnectionRepository::with_cipher(pool.clone(), Arc::clone(&cipher)),
    );
    let bank_connection = BankConnection::new(
        account.id,
        user_id,
        BankProvider::Monobank,
        "monobank-token".to_string(),
        "mono-e2e-account".to_string(),
    );
    bank_connections.create(&bank_connection).await?;

    let transactions: Arc<dyn TransactionRepository> =
        Arc::new(SqliteTransactionRepository::new(pool.clone()));
    let matcher = Arc::new(MatchChargesUseCase::new(
        Arc::clone(&charges),
        Arc::clone(&subscriptions),
        Arc::clone(&transactions),
        Arc::clone(&accounts),
        Arc::clone(&fx),
    ));
    let monobank = MonobankService::new(
        Arc::clone(&bank_connections),
        Arc::clone(&transactions),
        Arc::clone(&accounts),
        Arc::new(NoopMonobankClient),
        "https://api.example.test".to_string(),
        Some(matcher),
    );
    let webhook_item = MonoStatementItem {
        id: "mono-webhook-e2e-1".to_string(),
        time: (charged_at + Duration::hours(2)).timestamp(),
        description: Some("Netflix".to_string()),
        mcc: 5815,
        amount: -63_960,
        operation_amount: -63_960,
        currency_code: 980,
        balance: 100_000,
        hold: false,
    };
    assert_eq!(
        monobank
            .handle_webhook("mono-e2e-account", &webhook_item)
            .await?,
        1
    );

    let matched = wait_for_automatic_match(&*charges, charge_id, user_id).await?;
    assert_eq!(matched.match_source, Some(ChargeMatchSource::Automatic));
    let transaction_id = matched
        .transaction_id
        .expect("automatic matcher linked the webhook transaction");
    let (transaction, _) = transactions
        .find_by_id(transaction_id, user_id)
        .await?
        .expect("webhook transaction exists");
    assert_eq!(transaction.amount, dec!(639.60));
    assert_eq!(transaction.currency, "UAH");
    assert_eq!(transaction.category_id, Some(category.id));

    let second_sync = subscription_service
        .sync_connection_for_user(gmail_connection.id, user_id, false)
        .await?;
    assert!(second_sync.is_empty());
    let charge_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM subscription_charges WHERE source_key=$1")
            .bind(&pending.source_key)
            .fetch_one(&pool)
            .await?;
    assert_eq!(charge_count, 1);
    let still_matched = charges
        .find_by_id(charge_id, user_id)
        .await?
        .expect("idempotent Gmail replay retained the original charge");
    assert_eq!(still_matched.transaction_id, Some(transaction_id));
    assert_eq!(still_matched.match_status, ChargeMatchStatus::Matched);

    Ok(())
}
