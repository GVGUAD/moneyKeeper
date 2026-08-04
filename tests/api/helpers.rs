use std::sync::Arc;

use axum_test::TestServer;
use sqlx::PgPool;
use uuid::Uuid;

use moneykeeper::api::state::AppState;
use moneykeeper::application::accounts::AccountService;
use moneykeeper::application::categories::CategoryService;
use moneykeeper::application::monobank::MonobankService;
use moneykeeper::application::subscription_matching::MatchChargesUseCase;
use moneykeeper::application::subscriptions::SubscriptionService;
use moneykeeper::application::transactions::TransactionService;
use moneykeeper::application::user_settings::UserSettingsService;
use moneykeeper::domain::monobank::MonobankApiClient;
use moneykeeper::infrastructure::account_repository::SqliteAccountRepository;
use moneykeeper::infrastructure::category_repository::SqliteCategoryRepository;
use moneykeeper::infrastructure::email::oauth::OAuthConfig;
use moneykeeper::infrastructure::email::parsers::ParserRegistry;
use moneykeeper::infrastructure::email_connection_repository::PgEmailConnectionRepository;
use moneykeeper::infrastructure::fx_rate_repository::PgFxRateRepository;
use moneykeeper::infrastructure::monobank_repository::PgBankConnectionRepository;
use moneykeeper::infrastructure::subscription_charge_repository::PgSubscriptionChargeRepository;
use moneykeeper::infrastructure::subscription_repository::PgSubscriptionRepository;
use moneykeeper::infrastructure::transaction_repository::SqliteTransactionRepository;
use moneykeeper::infrastructure::user_settings_repository::PgUserSettingsRepository;

/// kid used in test JWTs and the test JWKS.
const TEST_KID: &str = "test-key-1";

/// P-256 test private key (PEM, PKCS8 format). Only used in tests — not a secret.
const TEST_EC_PRIVATE_KEY: &str = "-----BEGIN PRIVATE KEY-----
MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgN/zCeuQq48O/tp5y
b50qbJqKns6bCt6JctzISKw2sPqhRANCAASdCO4KOpDloBoTURgT/ZeiWey7OSIG
46TJaP8IugkOaxHZ6HCuZvK4AaDOXLZHOyRHEWK5AhPl1f98M4xYBkQy
-----END PRIVATE KEY-----";

/// Build a JwkSet containing the test public key so the app can verify test JWTs.
fn test_jwks() -> jsonwebtoken::jwk::JwkSet {
    // x/y are the base64url-encoded coordinates of the public point for TEST_EC_PRIVATE_KEY.
    let jwk_json = serde_json::json!({
        "keys": [{
            "kty": "EC",
            "crv": "P-256",
            "kid": TEST_KID,
            "alg": "ES256",
            "use": "sig",
            "x": "nQjuCjqQ5aAaE1EYE_2XolnsuzkiBuOkyWj_CLoJDms",
            "y": "EdnocK5m8rgBoM5ctkc7JEcRYrkCE-XV_3wzjFgGRDI"
        }]
    });
    serde_json::from_value(jwk_json).unwrap()
}

/// Generate a (user_id, JWT) pair for use in test requests.
pub fn create_test_user() -> (Uuid, String) {
    let user_id = Uuid::new_v4();
    let token = test_jwt(user_id);
    (user_id, token)
}

pub fn test_jwt(user_id: Uuid) -> String {
    use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};

    #[derive(serde::Serialize)]
    struct TestClaims {
        sub: String,
        aud: String,
        role: String,
        exp: i64,
        iat: i64,
    }

    let now = chrono::Utc::now().timestamp();
    let claims = TestClaims {
        sub: user_id.to_string(),
        aud: "authenticated".to_string(),
        role: "authenticated".to_string(),
        exp: now + 3600,
        iat: now,
    };
    let mut header = Header::new(Algorithm::ES256);
    header.kid = Some(TEST_KID.to_string());
    encode(
        &header,
        &claims,
        &EncodingKey::from_ec_pem(TEST_EC_PRIVATE_KEY.as_bytes()).unwrap(),
    )
    .unwrap()
}

pub async fn make_app_with_client(
    pool: PgPool,
    monobank_client: Arc<dyn MonobankApiClient>,
) -> TestServer {
    let tx_repo: Arc<dyn moneykeeper::domain::transaction::TransactionRepository> =
        Arc::new(SqliteTransactionRepository::new(pool.clone()));
    let account_repo: Arc<dyn moneykeeper::domain::account::AccountRepository> =
        Arc::new(SqliteAccountRepository::new(pool.clone()));
    let connection_repo: Arc<dyn moneykeeper::domain::bank_connection::BankConnectionRepository> =
        Arc::new(PgBankConnectionRepository::new(pool.clone()));
    let fx_repo: Arc<dyn moneykeeper::domain::fx_rate::FxRateRepository> =
        Arc::new(PgFxRateRepository::new(pool.clone()));
    let user_settings_repo: Arc<dyn moneykeeper::domain::user_settings::UserSettingsRepository> =
        Arc::new(PgUserSettingsRepository::new(pool.clone()));

    let email_conn_repo: Arc<dyn moneykeeper::domain::email_connection::EmailConnectionRepository> =
        Arc::new(PgEmailConnectionRepository::new(pool.clone()));
    let subscription_repo: Arc<dyn moneykeeper::domain::subscription::SubscriptionRepository> =
        Arc::new(PgSubscriptionRepository::new(pool.clone()));
    let charge_repo: Arc<
        dyn moneykeeper::domain::subscription_charge::SubscriptionChargeRepository,
    > = Arc::new(PgSubscriptionChargeRepository::new(pool.clone()));

    let parsers = Arc::new(ParserRegistry::default_set());

    // Use a no-op email fetcher for tests
    struct NoopFetcher;
    #[async_trait::async_trait]
    impl moneykeeper::domain::email::EmailFetcher for NoopFetcher {
        async fn fetch_new(
            &self,
            _conn: &moneykeeper::domain::email_connection::EmailConnection,
        ) -> anyhow::Result<(Vec<moneykeeper::domain::email::RawEmail>, Option<String>)> {
            Ok((vec![], None))
        }
    }

    let subscription_service = Arc::new(SubscriptionService::new(
        Arc::clone(&email_conn_repo),
        Arc::clone(&subscription_repo),
        Arc::clone(&charge_repo),
        Arc::new(NoopFetcher),
        parsers,
    ));

    let matcher = Arc::new(MatchChargesUseCase {
        charges: Arc::clone(&charge_repo),
        subscriptions: Arc::clone(&subscription_repo),
        transactions: Arc::clone(&tx_repo),
        accounts: Arc::clone(&account_repo),
        fx: Arc::clone(&fx_repo),
    });

    let oauth_config = Arc::new(OAuthConfig {
        client_id: "test-client-id".to_string(),
        client_secret: "test-client-secret".to_string(),
        redirect_uri: "http://localhost:3000/me/email-connections/gmail/oauth/callback".to_string(),
    });

    let state = AppState {
        accounts: Arc::new(AccountService::new(Arc::clone(&account_repo))),
        transactions: Arc::new(TransactionService::new(
            Arc::clone(&tx_repo),
            Arc::clone(&account_repo),
            Arc::clone(&connection_repo),
        )),
        categories: Arc::new(CategoryService::new(Arc::new(
            SqliteCategoryRepository::new(pool.clone()),
        ))),
        monobank: Arc::new(MonobankService::new(
            Arc::clone(&connection_repo),
            tx_repo,
            Arc::clone(&account_repo),
            monobank_client,
            "http://localhost:3000".to_string(),
            Some(Arc::clone(&matcher)),
        )),
        user_settings: Arc::new(UserSettingsService::new(
            Arc::clone(&user_settings_repo),
            Arc::clone(&fx_repo),
        )),
        supabase_jwks: Arc::new(test_jwks()),
        subscriptions: Arc::clone(&subscription_service),
        matcher: Arc::clone(&matcher),
        fx: Arc::clone(&fx_repo),
        gmail_oauth: oauth_config,
    };
    TestServer::new(moneykeeper::api::routes::router(state)).unwrap()
}

pub async fn make_app(pool: PgPool) -> TestServer {
    make_app_with_client(pool, MockMonobankClient::empty()).await
}

/// Extended test context that exposes service refs for direct manipulation in tests.
pub struct TestContext {
    pub server: TestServer,
    pub subscriptions: Arc<SubscriptionService>,
    pub matcher: Arc<MatchChargesUseCase>,
}

pub async fn make_app_ctx(pool: PgPool) -> TestContext {
    let tx_repo: Arc<dyn moneykeeper::domain::transaction::TransactionRepository> =
        Arc::new(SqliteTransactionRepository::new(pool.clone()));
    let account_repo: Arc<dyn moneykeeper::domain::account::AccountRepository> =
        Arc::new(SqliteAccountRepository::new(pool.clone()));
    let connection_repo: Arc<dyn moneykeeper::domain::bank_connection::BankConnectionRepository> =
        Arc::new(PgBankConnectionRepository::new(pool.clone()));
    let fx_repo: Arc<dyn moneykeeper::domain::fx_rate::FxRateRepository> =
        Arc::new(PgFxRateRepository::new(pool.clone()));
    let user_settings_repo: Arc<dyn moneykeeper::domain::user_settings::UserSettingsRepository> =
        Arc::new(PgUserSettingsRepository::new(pool.clone()));

    let email_conn_repo: Arc<dyn moneykeeper::domain::email_connection::EmailConnectionRepository> =
        Arc::new(PgEmailConnectionRepository::new(pool.clone()));
    let subscription_repo: Arc<dyn moneykeeper::domain::subscription::SubscriptionRepository> =
        Arc::new(PgSubscriptionRepository::new(pool.clone()));
    let charge_repo: Arc<
        dyn moneykeeper::domain::subscription_charge::SubscriptionChargeRepository,
    > = Arc::new(PgSubscriptionChargeRepository::new(pool.clone()));

    let parsers = Arc::new(ParserRegistry::default_set());

    struct NoopFetcher;
    #[async_trait::async_trait]
    impl moneykeeper::domain::email::EmailFetcher for NoopFetcher {
        async fn fetch_new(
            &self,
            _conn: &moneykeeper::domain::email_connection::EmailConnection,
        ) -> anyhow::Result<(Vec<moneykeeper::domain::email::RawEmail>, Option<String>)> {
            Ok((vec![], None))
        }
    }

    let subscription_service = Arc::new(SubscriptionService::new(
        Arc::clone(&email_conn_repo),
        Arc::clone(&subscription_repo),
        Arc::clone(&charge_repo),
        Arc::new(NoopFetcher),
        parsers,
    ));

    let matcher = Arc::new(MatchChargesUseCase {
        charges: Arc::clone(&charge_repo),
        subscriptions: Arc::clone(&subscription_repo),
        transactions: Arc::clone(&tx_repo),
        accounts: Arc::clone(&account_repo),
        fx: Arc::clone(&fx_repo),
    });

    let oauth_config = Arc::new(OAuthConfig {
        client_id: "test-client-id".to_string(),
        client_secret: "test-client-secret".to_string(),
        redirect_uri: "http://localhost:3000/me/email-connections/gmail/oauth/callback".to_string(),
    });

    let monobank_client = MockMonobankClient::empty();

    let state = AppState {
        accounts: Arc::new(AccountService::new(Arc::clone(&account_repo))),
        transactions: Arc::new(TransactionService::new(
            Arc::clone(&tx_repo),
            Arc::clone(&account_repo),
            Arc::clone(&connection_repo),
        )),
        categories: Arc::new(CategoryService::new(Arc::new(
            SqliteCategoryRepository::new(pool.clone()),
        ))),
        monobank: Arc::new(MonobankService::new(
            Arc::clone(&connection_repo),
            tx_repo,
            Arc::clone(&account_repo),
            monobank_client,
            "http://localhost:3000".to_string(),
            Some(Arc::clone(&matcher)),
        )),
        user_settings: Arc::new(UserSettingsService::new(
            Arc::clone(&user_settings_repo),
            Arc::clone(&fx_repo),
        )),
        supabase_jwks: Arc::new(test_jwks()),
        subscriptions: Arc::clone(&subscription_service),
        matcher: Arc::clone(&matcher),
        fx: Arc::clone(&fx_repo),
        gmail_oauth: oauth_config,
    };

    TestContext {
        server: TestServer::new(moneykeeper::api::routes::router(state)).unwrap(),
        subscriptions: subscription_service,
        matcher,
    }
}

pub struct MockMonobankClient {
    pub accounts: Vec<moneykeeper::domain::monobank::MonoAccount>,
    pub statement_items: Vec<moneykeeper::domain::monobank::MonoStatementItem>,
}

impl MockMonobankClient {
    pub fn empty() -> Arc<Self> {
        Arc::new(Self {
            accounts: vec![],
            statement_items: vec![],
        })
    }

    #[allow(dead_code)]
    pub fn with_accounts(accounts: Vec<moneykeeper::domain::monobank::MonoAccount>) -> Arc<Self> {
        Arc::new(Self {
            accounts,
            statement_items: vec![],
        })
    }
}

#[async_trait::async_trait]
impl moneykeeper::domain::monobank::MonobankApiClient for MockMonobankClient {
    async fn get_accounts(
        &self,
        _token: &str,
    ) -> anyhow::Result<Vec<moneykeeper::domain::monobank::MonoAccount>> {
        Ok(self.accounts.clone())
    }

    async fn get_statement(
        &self,
        _token: &str,
        _acc: &str,
        _from: chrono::DateTime<chrono::Utc>,
        _to: chrono::DateTime<chrono::Utc>,
    ) -> anyhow::Result<Vec<moneykeeper::domain::monobank::MonoStatementItem>> {
        Ok(self.statement_items.clone())
    }

    async fn set_webhook(&self, _token: &str, _url: &str) -> anyhow::Result<()> {
        Ok(())
    }
}

/// Creates a default Cash/USD account for the given user token. Returns the account UUID.
pub async fn create_account_for(server: &TestServer, token: &str) -> Uuid {
    let (h, v) = auth(token);
    let res = server
        .post("/accounts")
        .add_header(h, v)
        .json(&serde_json::json!({
            "name": "Test Account",
            "account_type": "Cash",
            "currency": "USD"
        }))
        .await;

    let body: serde_json::Value = res.json();
    Uuid::parse_str(body["id"].as_str().unwrap()).unwrap()
}

/// Returns an (Authorization header name, Bearer value) tuple for use with add_header.
pub fn auth(token: &str) -> (axum::http::HeaderName, axum::http::HeaderValue) {
    (
        axum::http::header::AUTHORIZATION,
        format!("Bearer {token}")
            .parse::<axum::http::HeaderValue>()
            .unwrap(),
    )
}

/// Creates a default category for the given user token. Returns the category UUID.
pub async fn create_category_for(server: &TestServer, token: &str) -> Uuid {
    let (h, v) = auth(token);
    let res = server
        .post("/categories")
        .add_header(h, v)
        .json(&serde_json::json!({ "name": "Test Category" }))
        .await;

    let body: serde_json::Value = res.json();
    Uuid::parse_str(body["id"].as_str().unwrap()).unwrap()
}

pub async fn seed_fx_rate(
    pool: &sqlx::PgPool,
    date: chrono::NaiveDate,
    from: &str,
    rate: rust_decimal::Decimal,
) {
    sqlx::query!(
        "INSERT INTO fx_rates (rate_date, from_currency, to_currency, rate)
         VALUES ($1, $2, 'UAH', $3)",
        date,
        from,
        rate,
    )
    .execute(pool)
    .await
    .expect("seed fx rate");
}
