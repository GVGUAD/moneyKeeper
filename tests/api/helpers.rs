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
use moneykeeper::infrastructure::credential_crypto::{SecretValue, TokenCipher, TokenCipherConfig};
use moneykeeper::infrastructure::email::oauth::{
    GmailOAuthClient, GmailOAuthService, GmailProfile, GmailProviderError, GmailTokenSet,
    OAuthConfig, PgGmailOAuthStateStore,
};
use moneykeeper::infrastructure::email::parsers::ParserRegistry;
use moneykeeper::infrastructure::email_connection_repository::PgEmailConnectionRepository;
use moneykeeper::infrastructure::email_sync_repository::PgEmailSyncRepository;
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

fn test_gmail_oauth(
    pool: &PgPool,
    connections: Arc<dyn moneykeeper::domain::email_connection::EmailConnectionRepository>,
    success_redirect_uri: Option<String>,
    failure_redirect_uri: Option<String>,
) -> Arc<GmailOAuthService> {
    struct FakeGmailOAuthClient;

    #[async_trait::async_trait]
    impl GmailOAuthClient for FakeGmailOAuthClient {
        fn authorization_url(&self, state: &str, challenge: &str) -> anyhow::Result<String> {
            Ok(format!(
                "https://accounts.example.test/authorize?state={state}&code_challenge={challenge}&code_challenge_method=S256"
            ))
        }

        async fn exchange_code(
            &self,
            code: &str,
            _pkce_verifier: &str,
        ) -> anyhow::Result<GmailTokenSet> {
            if code != "valid-code" {
                return Err(GmailProviderError::Rejected.into());
            }
            Ok(GmailTokenSet {
                access_token: SecretValue::new("test-access-token"),
                refresh_token: Some(SecretValue::new("test-refresh-token")),
                expires_at: chrono::Utc::now() + chrono::Duration::hours(1),
            })
        }

        async fn profile(&self, access_token: &str) -> anyhow::Result<GmailProfile> {
            if access_token != "test-access-token" {
                return Err(GmailProviderError::InvalidCredentials.into());
            }
            Ok(GmailProfile {
                email_address: " OAuth-API@Example.COM ".to_string(),
                verified: true,
            })
        }

        async fn refresh(&self, _refresh_token: &str) -> anyhow::Result<GmailTokenSet> {
            Ok(GmailTokenSet {
                access_token: SecretValue::new("test-refreshed-access-token"),
                refresh_token: None,
                expires_at: chrono::Utc::now() + chrono::Duration::hours(1),
            })
        }

        async fn revoke(&self, _token: &str) -> anyhow::Result<()> {
            Ok(())
        }
    }

    let cipher = Arc::new(TokenCipher::new(TokenCipherConfig {
        active_key_id: "test".to_string(),
        keys: [("test".to_string(), [42_u8; 32])].into_iter().collect(),
    }));
    let config = OAuthConfig::new(
        "test-client-id".to_string(),
        "test-client-secret",
        "http://localhost:3000/oauth/gmail/callback".to_string(),
    )
    .with_result_redirects(success_redirect_uri, failure_redirect_uri);
    Arc::new(GmailOAuthService::new(
        Arc::new(FakeGmailOAuthClient),
        Arc::new(PgGmailOAuthStateStore::new(pool.clone(), cipher)),
        connections,
        config,
    ))
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

async fn make_app_with_client_and_oauth_redirects(
    pool: PgPool,
    monobank_client: Arc<dyn MonobankApiClient>,
    success_redirect_uri: Option<String>,
    failure_redirect_uri: Option<String>,
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
    let category_service = Arc::new(CategoryService::new(Arc::new(
        SqliteCategoryRepository::new(pool.clone()),
    )));

    // Use a no-op email fetcher for tests
    struct NoopFetcher;
    #[async_trait::async_trait]
    impl moneykeeper::domain::email::EmailFetcher for NoopFetcher {
        async fn fetch_new(
            &self,
            _conn: &moneykeeper::domain::email_connection::EmailConnection,
        ) -> anyhow::Result<moneykeeper::domain::email::EmailFetchBatch> {
            Ok(moneykeeper::domain::email::EmailFetchBatch {
                emails: vec![],
                failures: vec![],
                ignored_message_ids: vec![],
                next_history_id: None,
                history_was_reset: false,
            })
        }
    }

    let gmail_oauth = test_gmail_oauth(
        &pool,
        Arc::clone(&email_conn_repo),
        success_redirect_uri,
        failure_redirect_uri,
    );
    let subscription_service = Arc::new(
        SubscriptionService::new(
            Arc::clone(&email_conn_repo),
            Arc::clone(&subscription_repo),
            Arc::clone(&charge_repo),
            Arc::new(NoopFetcher),
            parsers,
        )
        .with_reliable_sync(
            Arc::new(PgEmailSyncRepository::new(pool.clone())),
            Arc::clone(gmail_oauth.client()),
        )
        .with_category_validation(Arc::clone(&category_service)),
    );

    let matcher = Arc::new(MatchChargesUseCase::new(
        Arc::clone(&charge_repo),
        Arc::clone(&subscription_repo),
        Arc::clone(&tx_repo),
        Arc::clone(&account_repo),
        Arc::clone(&fx_repo),
    ));

    let state = AppState {
        accounts: Arc::new(AccountService::new(Arc::clone(&account_repo))),
        transactions: Arc::new(TransactionService::new(
            Arc::clone(&tx_repo),
            Arc::clone(&account_repo),
            Arc::clone(&connection_repo),
        )),
        categories: category_service,
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
        gmail_oauth,
    };
    TestServer::new(moneykeeper::api::routes::router(state)).unwrap()
}

pub async fn make_app_with_client(
    pool: PgPool,
    monobank_client: Arc<dyn MonobankApiClient>,
) -> TestServer {
    make_app_with_client_and_oauth_redirects(pool, monobank_client, None, None).await
}

pub async fn make_app_with_oauth_redirects(
    pool: PgPool,
    success_redirect_uri: Option<String>,
    failure_redirect_uri: Option<String>,
) -> TestServer {
    make_app_with_client_and_oauth_redirects(
        pool,
        MockMonobankClient::empty(),
        success_redirect_uri,
        failure_redirect_uri,
    )
    .await
}

pub async fn make_app(pool: PgPool) -> TestServer {
    make_app_with_client(pool, MockMonobankClient::empty()).await
}

/// Extended test context that exposes service refs for direct manipulation in tests.
pub struct TestContext {
    pub server: TestServer,
    pub subscription_repo: Arc<dyn moneykeeper::domain::subscription::SubscriptionRepository>,
    pub charge_repo:
        Arc<dyn moneykeeper::domain::subscription_charge::SubscriptionChargeRepository>,
    pub email_connection_repo:
        Arc<dyn moneykeeper::domain::email_connection::EmailConnectionRepository>,
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
    let category_service = Arc::new(CategoryService::new(Arc::new(
        SqliteCategoryRepository::new(pool.clone()),
    )));

    struct NoopFetcher;
    #[async_trait::async_trait]
    impl moneykeeper::domain::email::EmailFetcher for NoopFetcher {
        async fn fetch_new(
            &self,
            _conn: &moneykeeper::domain::email_connection::EmailConnection,
        ) -> anyhow::Result<moneykeeper::domain::email::EmailFetchBatch> {
            Ok(moneykeeper::domain::email::EmailFetchBatch {
                emails: vec![],
                failures: vec![],
                ignored_message_ids: vec![],
                next_history_id: None,
                history_was_reset: false,
            })
        }
    }

    let gmail_oauth = test_gmail_oauth(&pool, Arc::clone(&email_conn_repo), None, None);
    let subscription_service = Arc::new(
        SubscriptionService::new(
            Arc::clone(&email_conn_repo),
            Arc::clone(&subscription_repo),
            Arc::clone(&charge_repo),
            Arc::new(NoopFetcher),
            parsers,
        )
        .with_reliable_sync(
            Arc::new(PgEmailSyncRepository::new(pool.clone())),
            Arc::clone(gmail_oauth.client()),
        )
        .with_category_validation(Arc::clone(&category_service)),
    );

    let matcher = Arc::new(MatchChargesUseCase::new(
        Arc::clone(&charge_repo),
        Arc::clone(&subscription_repo),
        Arc::clone(&tx_repo),
        Arc::clone(&account_repo),
        Arc::clone(&fx_repo),
    ));

    let monobank_client = MockMonobankClient::empty();

    let state = AppState {
        accounts: Arc::new(AccountService::new(Arc::clone(&account_repo))),
        transactions: Arc::new(TransactionService::new(
            Arc::clone(&tx_repo),
            Arc::clone(&account_repo),
            Arc::clone(&connection_repo),
        )),
        categories: category_service,
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
        gmail_oauth,
    };

    TestContext {
        server: TestServer::new(moneykeeper::api::routes::router(state)).unwrap(),
        subscription_repo,
        charge_repo,
        email_connection_repo: email_conn_repo,
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
