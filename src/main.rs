use std::sync::Arc;
use tracing_subscriber::EnvFilter;

use moneykeeper::api;
use moneykeeper::api::state::AppState;
use moneykeeper::application::accounts::AccountService;
use moneykeeper::application::categories::CategoryService;
use moneykeeper::application::fx_sync::FxSyncUseCase;
use moneykeeper::application::monobank::MonobankService;
use moneykeeper::application::subscription_lifecycle::DetectLapsedUseCase;
use moneykeeper::application::subscription_matching::MatchChargesUseCase;
use moneykeeper::application::subscriptions::SubscriptionService;
use moneykeeper::application::transactions::TransactionService;
use moneykeeper::application::user_settings::UserSettingsService;
use moneykeeper::infrastructure::account_repository::SqliteAccountRepository;
use moneykeeper::infrastructure::category_repository::SqliteCategoryRepository;
use moneykeeper::infrastructure::credential_crypto::TokenCipher;
use moneykeeper::infrastructure::db::create_pool;
use moneykeeper::infrastructure::email::gmail_client::GmailClient;
use moneykeeper::infrastructure::email::oauth::{
    GmailOAuthClient, GmailOAuthService, OAuthConfig, PgGmailOAuthStateStore,
    ReqwestGmailOAuthClient,
};
use moneykeeper::infrastructure::email::parsers::ParserRegistry;
use moneykeeper::infrastructure::email_connection_repository::PgEmailConnectionRepository;
use moneykeeper::infrastructure::email_sync_repository::PgEmailSyncRepository;
use moneykeeper::infrastructure::fx_rate_repository::PgFxRateRepository;
use moneykeeper::infrastructure::monobank_client::ReqwestMonobankClient;
use moneykeeper::infrastructure::monobank_repository::PgBankConnectionRepository;
use moneykeeper::infrastructure::nbu_client::NbuFxRateSource;
use moneykeeper::infrastructure::subscription_charge_repository::PgSubscriptionChargeRepository;
use moneykeeper::infrastructure::subscription_repository::PgSubscriptionRepository;
use moneykeeper::infrastructure::transaction_repository::SqliteTransactionRepository;
use moneykeeper::infrastructure::user_settings_repository::PgUserSettingsRepository;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    let supabase_url = std::env::var("SUPABASE_URL").expect("SUPABASE_URL must be set");
    let bind_addr = std::env::var("BIND_ADDR").unwrap_or_else(|_| "0.0.0.0:8080".to_string());
    let public_url =
        std::env::var("PUBLIC_URL").unwrap_or_else(|_| "http://localhost:3000".to_string());

    let gmail_client_id = std::env::var("GMAIL_CLIENT_ID").expect("GMAIL_CLIENT_ID must be set");
    let gmail_client_secret =
        std::env::var("GMAIL_CLIENT_SECRET").expect("GMAIL_CLIENT_SECRET must be set");
    let gmail_redirect_uri = std::env::var("GMAIL_REDIRECT_URI")
        .unwrap_or_else(|_| format!("{public_url}/oauth/gmail/callback"));
    // Credential configuration is mandatory: validate it before any external
    // startup work so a rolling deploy cannot fall back to plaintext writes.
    let token_cipher = Arc::new(TokenCipher::from_env()?);

    let jwks_url = format!(
        "{}/auth/v1/.well-known/jwks.json",
        supabase_url.trim_end_matches('/')
    );
    tracing::info!("fetching JWKS from {jwks_url}");
    let jwks: jsonwebtoken::jwk::JwkSet = reqwest::get(&jwks_url).await?.json().await?;

    let pool = create_pool(&database_url).await?;

    let account_repo: Arc<dyn moneykeeper::domain::account::AccountRepository> =
        Arc::new(SqliteAccountRepository::new(pool.clone()));
    let transaction_repo: Arc<dyn moneykeeper::domain::transaction::TransactionRepository> =
        Arc::new(SqliteTransactionRepository::new(pool.clone()));

    let fx_repo: Arc<dyn moneykeeper::domain::fx_rate::FxRateRepository> =
        Arc::new(PgFxRateRepository::new(pool.clone()));
    let fx_sync = Arc::new(FxSyncUseCase::new(
        Arc::new(NbuFxRateSource::new()),
        Arc::clone(&fx_repo),
    ));
    let user_settings_repo: Arc<dyn moneykeeper::domain::user_settings::UserSettingsRepository> =
        Arc::new(PgUserSettingsRepository::new(pool.clone()));
    let user_settings_service = Arc::new(UserSettingsService::new(
        Arc::clone(&user_settings_repo),
        Arc::clone(&fx_repo),
    ));

    let connection_repo: Arc<dyn moneykeeper::domain::bank_connection::BankConnectionRepository> =
        Arc::new(PgBankConnectionRepository::with_cipher(
            pool.clone(),
            Arc::clone(&token_cipher),
        ));

    let email_conn_repo: Arc<dyn moneykeeper::domain::email_connection::EmailConnectionRepository> =
        Arc::new(PgEmailConnectionRepository::with_cipher(
            pool.clone(),
            Arc::clone(&token_cipher),
        ));
    let email_sync_repo: Arc<dyn moneykeeper::domain::email_sync::EmailSyncRepository> =
        Arc::new(PgEmailSyncRepository::new(pool.clone()));
    let subscription_repo: Arc<dyn moneykeeper::domain::subscription::SubscriptionRepository> =
        Arc::new(PgSubscriptionRepository::new(pool.clone()));
    let charge_repo: Arc<
        dyn moneykeeper::domain::subscription_charge::SubscriptionChargeRepository,
    > = Arc::new(PgSubscriptionChargeRepository::new(pool.clone()));

    let gmail_client = Arc::new(GmailClient::production());
    let parsers = Arc::new(ParserRegistry::default_set());
    let category_service = Arc::new(CategoryService::new(Arc::new(
        SqliteCategoryRepository::new(pool.clone()),
    )));

    let oauth_config = OAuthConfig::new(gmail_client_id, gmail_client_secret, gmail_redirect_uri)
        .with_result_redirects(
            std::env::var("GMAIL_OAUTH_SUCCESS_URL").ok(),
            std::env::var("GMAIL_OAUTH_FAILURE_URL").ok(),
        );
    let gmail_oauth_client: Arc<dyn GmailOAuthClient> =
        Arc::new(ReqwestGmailOAuthClient::new(oauth_config.clone()));
    let gmail_oauth = Arc::new(GmailOAuthService::new(
        Arc::clone(&gmail_oauth_client),
        Arc::new(PgGmailOAuthStateStore::new(
            pool.clone(),
            Arc::clone(&token_cipher),
        )),
        Arc::clone(&email_conn_repo),
        oauth_config,
    ));

    let subscription_service = Arc::new(
        SubscriptionService::new(
            Arc::clone(&email_conn_repo),
            Arc::clone(&subscription_repo),
            Arc::clone(&charge_repo),
            gmail_client.clone(),
            parsers.clone(),
        )
        .with_reliable_sync(Arc::clone(&email_sync_repo), gmail_oauth_client)
        .with_category_validation(Arc::clone(&category_service)),
    );

    let matcher = Arc::new(MatchChargesUseCase::new(
        Arc::clone(&charge_repo),
        Arc::clone(&subscription_repo),
        Arc::clone(&transaction_repo),
        Arc::clone(&account_repo),
        Arc::clone(&fx_repo),
    ));

    let lifecycle = Arc::new(DetectLapsedUseCase {
        subscriptions: Arc::clone(&subscription_repo),
    });

    let monobank_service = Arc::new(MonobankService::new(
        Arc::clone(&connection_repo),
        Arc::clone(&transaction_repo),
        Arc::clone(&account_repo),
        Arc::new(ReqwestMonobankClient::new()),
        public_url,
        Some(Arc::clone(&matcher)),
    ));

    let state = AppState {
        accounts: Arc::new(AccountService::new(Arc::clone(&account_repo))),
        transactions: Arc::new(TransactionService::new(
            Arc::clone(&transaction_repo),
            Arc::clone(&account_repo),
            Arc::clone(&connection_repo),
        )),
        categories: category_service,
        monobank: monobank_service.clone(),
        user_settings: Arc::clone(&user_settings_service),
        supabase_jwks: Arc::new(jwks),
        subscriptions: Arc::clone(&subscription_service),
        matcher: Arc::clone(&matcher),
        fx: Arc::clone(&fx_repo),
        gmail_oauth,
    };

    monobank_service.restart_incomplete_syncs().await;

    // Seed every charge date reachable by Gmail's 30-day receipt lookback,
    // then catch up once a day. Matching skips genuinely missing quotes, while
    // this scheduler makes a fresh deployment useful without manual seeding.
    {
        let fx_sync = Arc::clone(&fx_sync);
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(std::time::Duration::from_secs(86_400));
            loop {
                ticker.tick().await;
                let today = chrono::Utc::now().date_naive();
                let lookback_start = today - chrono::Duration::days(30);
                if let Err(error) = fx_sync.backfill_missing(lookback_start, today).await {
                    tracing::warn!(from=%lookback_start, %today, ?error, "FX catch-up failed");
                }
            }
        });
    }

    // Poll due connections every minute. Database leases make this safe across
    // schedulers, manual resyncs, and multiple application replicas.
    {
        let subs = Arc::clone(&subscription_service);
        let matcher = Arc::clone(&matcher);
        let conns = Arc::clone(&email_conn_repo);
        let sync_repo = Arc::clone(&email_sync_repo);
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(std::time::Duration::from_secs(60));
            let permits = Arc::new(tokio::sync::Semaphore::new(4));
            loop {
                ticker.tick().await;
                let available = permits.available_permits();
                if available == 0 {
                    continue;
                }
                let due = match sync_repo
                    .list_due_connection_ids(chrono::Utc::now(), available as i64)
                    .await
                {
                    Ok(ids) => ids.into_iter().collect::<std::collections::HashSet<_>>(),
                    Err(e) => {
                        tracing::warn!("scheduler: list due email connections failed: {e:?}");
                        continue;
                    }
                };
                let connections: Vec<_> = match conns.list_connected().await {
                    Ok(c) => c
                        .into_iter()
                        .filter(|conn| due.contains(&conn.id))
                        .collect(),
                    Err(e) => {
                        tracing::warn!("scheduler: list_connected failed: {e:?}");
                        continue;
                    }
                };
                for conn in connections {
                    let Ok(permit) = Arc::clone(&permits).try_acquire_owned() else {
                        break;
                    };
                    let subs = Arc::clone(&subs);
                    let matcher = Arc::clone(&matcher);
                    tokio::spawn(async move {
                        let _permit = permit;
                        match subs.sync_connection(conn.id).await {
                            Ok(_) => {
                                if let Err(e) = matcher.run_for_user(conn.user_id).await {
                                    tracing::warn!(
                                        "matcher failed for user {}: {e:?}",
                                        conn.user_id
                                    );
                                }
                            }
                            Err(e) => tracing::warn!("sync failed for conn {}: {e:?}", conn.id),
                        }
                    });
                }
            }
        });
    }

    // Ensure pending charges age after seven days even when their Gmail
    // connection needs reconnection and no bank webhook happens to arrive.
    {
        let charges = Arc::clone(&charge_repo);
        let matcher = Arc::clone(&matcher);
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(std::time::Duration::from_secs(3_600));
            loop {
                ticker.tick().await;
                let user_ids = match charges.list_users_with_pending().await {
                    Ok(user_ids) => user_ids,
                    Err(error) => {
                        tracing::warn!(?error, "failed to list users with pending charges");
                        continue;
                    }
                };
                for user_id in user_ids {
                    if let Err(error) = matcher.run_for_user(user_id).await {
                        tracing::warn!(%user_id, ?error, "scheduled pending-charge matching failed");
                    }
                }
            }
        });
    }

    // Daily lapse detection scheduler
    {
        let lifecycle = Arc::clone(&lifecycle);
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(std::time::Duration::from_secs(86_400));
            loop {
                ticker.tick().await;
                if let Err(e) = lifecycle.run().await {
                    tracing::warn!("lapse-detection failed: {e:?}");
                }
            }
        });
    }

    let router = api::routes::router(state);
    let listener = tokio::net::TcpListener::bind(&bind_addr).await?;
    tracing::info!("listening on {bind_addr}");
    axum::serve(listener, router).await?;
    Ok(())
}
