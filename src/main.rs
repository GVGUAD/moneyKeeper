use std::sync::Arc;
use tracing_subscriber::EnvFilter;

use moneykeeper::api;
use moneykeeper::api::state::AppState;
use moneykeeper::application::accounts::AccountService;
use moneykeeper::application::categories::CategoryService;
use moneykeeper::application::monobank::MonobankService;
use moneykeeper::application::transactions::TransactionService;
use moneykeeper::infrastructure::account_repository::SqliteAccountRepository;
use moneykeeper::infrastructure::category_repository::SqliteCategoryRepository;
use moneykeeper::infrastructure::db::create_pool;
use moneykeeper::infrastructure::monobank_client::ReqwestMonobankClient;
use moneykeeper::infrastructure::monobank_repository::PgBankConnectionRepository;
use moneykeeper::infrastructure::transaction_repository::SqliteTransactionRepository;

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

    let monobank_service = Arc::new(MonobankService::new(
        Arc::new(PgBankConnectionRepository::new(pool.clone())),
        Arc::clone(&transaction_repo),
        Arc::clone(&account_repo),
        Arc::new(ReqwestMonobankClient::new()),
        public_url,
    ));

    let state = AppState {
        accounts: Arc::new(AccountService::new(Arc::clone(&account_repo))),
        transactions: Arc::new(TransactionService::new(
            Arc::clone(&transaction_repo),
            Arc::clone(&account_repo),
        )),
        categories: Arc::new(CategoryService::new(Arc::new(
            SqliteCategoryRepository::new(pool.clone()),
        ))),
        monobank: monobank_service.clone(),
        supabase_jwks: Arc::new(jwks),
    };

    monobank_service.restart_incomplete_syncs().await;

    let router = api::routes::router(state);
    let listener = tokio::net::TcpListener::bind(&bind_addr).await?;
    tracing::info!("listening on {bind_addr}");
    axum::serve(listener, router).await?;
    Ok(())
}
