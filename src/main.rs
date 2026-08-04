use std::sync::Arc;
use tracing_subscriber::EnvFilter;

use moneykeeper::api;
use moneykeeper::api::state::AppState;
use moneykeeper::application::accounts::AccountService;
use moneykeeper::application::categories::CategoryService;
use moneykeeper::application::monobank::MonobankService;
use moneykeeper::application::subscription_lifecycle::DetectLapsedUseCase;
use moneykeeper::application::subscription_matching::MatchChargesUseCase;
use moneykeeper::application::subscriptions::SubscriptionService;
use moneykeeper::application::transactions::TransactionService;
use moneykeeper::application::user_settings::UserSettingsService;
use moneykeeper::infrastructure::account_repository::SqliteAccountRepository;
use moneykeeper::infrastructure::category_repository::SqliteCategoryRepository;
use moneykeeper::infrastructure::db::create_pool;
use moneykeeper::infrastructure::email::gmail_client::GmailClient;
use moneykeeper::infrastructure::email::oauth::OAuthConfig;
use moneykeeper::infrastructure::email::parsers::ParserRegistry;
use moneykeeper::infrastructure::email_connection_repository::PgEmailConnectionRepository;
use moneykeeper::infrastructure::fx_rate_repository::PgFxRateRepository;
use moneykeeper::infrastructure::monobank_client::ReqwestMonobankClient;
use moneykeeper::infrastructure::monobank_repository::PgBankConnectionRepository;
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
        .unwrap_or_else(|_| format!("{public_url}/me/email-connections/gmail/oauth/callback"));

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
    let user_settings_repo: Arc<dyn moneykeeper::domain::user_settings::UserSettingsRepository> =
        Arc::new(PgUserSettingsRepository::new(pool.clone()));
    let user_settings_service = Arc::new(UserSettingsService::new(
        Arc::clone(&user_settings_repo),
        Arc::clone(&fx_repo),
    ));

    let connection_repo: Arc<dyn moneykeeper::domain::bank_connection::BankConnectionRepository> =
        Arc::new(PgBankConnectionRepository::new(pool.clone()));

    let email_conn_repo: Arc<dyn moneykeeper::domain::email_connection::EmailConnectionRepository> =
        Arc::new(PgEmailConnectionRepository::new(pool.clone()));
    let subscription_repo: Arc<dyn moneykeeper::domain::subscription::SubscriptionRepository> =
        Arc::new(PgSubscriptionRepository::new(pool.clone()));
    let charge_repo: Arc<
        dyn moneykeeper::domain::subscription_charge::SubscriptionChargeRepository,
    > = Arc::new(PgSubscriptionChargeRepository::new(pool.clone()));

    let gmail_client = Arc::new(GmailClient::new(
        gmail_client_id.clone(),
        gmail_client_secret.clone(),
    ));
    let parsers = Arc::new(ParserRegistry::default_set());

    let subscription_service = Arc::new(SubscriptionService::new(
        Arc::clone(&email_conn_repo),
        Arc::clone(&subscription_repo),
        Arc::clone(&charge_repo),
        gmail_client.clone(),
        parsers.clone(),
    ));

    let matcher = Arc::new(MatchChargesUseCase {
        charges: Arc::clone(&charge_repo),
        subscriptions: Arc::clone(&subscription_repo),
        transactions: Arc::clone(&transaction_repo),
        accounts: Arc::clone(&account_repo),
        fx: Arc::clone(&fx_repo),
    });

    let lifecycle = Arc::new(DetectLapsedUseCase {
        subscriptions: Arc::clone(&subscription_repo),
    });

    let oauth_config = Arc::new(OAuthConfig {
        client_id: gmail_client_id,
        client_secret: gmail_client_secret,
        redirect_uri: gmail_redirect_uri,
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
        categories: Arc::new(CategoryService::new(Arc::new(
            SqliteCategoryRepository::new(pool.clone()),
        ))),
        monobank: monobank_service.clone(),
        user_settings: Arc::clone(&user_settings_service),
        supabase_jwks: Arc::new(jwks),
        subscriptions: Arc::clone(&subscription_service),
        matcher: Arc::clone(&matcher),
        fx: Arc::clone(&fx_repo),
        gmail_oauth: oauth_config,
    };

    monobank_service.restart_incomplete_syncs().await;

    // Hourly email sync scheduler
    {
        let subs = Arc::clone(&subscription_service);
        let matcher = Arc::clone(&matcher);
        let conns = Arc::clone(&email_conn_repo);
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(std::time::Duration::from_secs(3600));
            loop {
                ticker.tick().await;
                let connections = match conns.list_connected().await {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::warn!("scheduler: list_connected failed: {e:?}");
                        continue;
                    }
                };
                for conn in connections {
                    match subs.sync_connection(conn.id).await {
                        Ok(new_ids) if !new_ids.is_empty() => {
                            if let Err(e) = matcher.run_for_user(conn.user_id).await {
                                tracing::warn!("matcher failed for user {}: {e:?}", conn.user_id);
                            }
                        }
                        Ok(_) => {}
                        Err(e) => {
                            tracing::warn!("sync failed for conn {}: {e:?}", conn.id);
                        }
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
