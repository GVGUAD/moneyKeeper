use std::sync::Arc;

use axum_test::TestServer;
use sqlx::PgPool;
use testcontainers::runners::AsyncRunner;
use testcontainers::ContainerAsync;
use testcontainers_modules::postgres::Postgres;
use uuid::Uuid;

use moneykeeper::api::state::AppState;
use moneykeeper::application::accounts::AccountService;
use moneykeeper::application::categories::CategoryService;
use moneykeeper::application::monobank::MonobankService;
use moneykeeper::application::transactions::TransactionService;
use moneykeeper::domain::monobank::MonobankApiClient;
use moneykeeper::infrastructure::account_repository::SqliteAccountRepository;
use moneykeeper::infrastructure::category_repository::SqliteCategoryRepository;
use moneykeeper::infrastructure::monobank_repository::PgBankConnectionRepository;
use moneykeeper::infrastructure::transaction_repository::SqliteTransactionRepository;

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

pub async fn spawn_postgres() -> (ContainerAsync<Postgres>, PgPool) {
    let container = Postgres::default().start().await.unwrap();
    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let url = format!("postgres://postgres:postgres@127.0.0.1:{}/postgres", port);
    let pool = PgPool::connect(&url).await.unwrap();
    sqlx::migrate!("src/infrastructure/migrations")
        .run(&pool)
        .await
        .unwrap();
    (container, pool)
}

pub async fn make_app_with_client(
    pool: PgPool,
    monobank_client: Arc<dyn MonobankApiClient>,
) -> TestServer {
    let tx_repo = Arc::new(SqliteTransactionRepository::new(pool.clone()));
    let state = AppState {
        accounts: Arc::new(AccountService::new(Arc::new(SqliteAccountRepository::new(
            pool.clone(),
        )))),
        transactions: Arc::new(TransactionService::new(tx_repo.clone())),
        categories: Arc::new(CategoryService::new(Arc::new(
            SqliteCategoryRepository::new(pool.clone()),
        ))),
        monobank: Arc::new(MonobankService::new(
            Arc::new(PgBankConnectionRepository::new(pool.clone())),
            tx_repo,
            monobank_client,
            "http://localhost:3000".to_string(),
        )),
        supabase_jwks: Arc::new(test_jwks()),
    };
    TestServer::new(moneykeeper::api::routes::router(state)).unwrap()
}

pub async fn make_app(pool: PgPool) -> TestServer {
    make_app_with_client(pool, MockMonobankClient::empty()).await
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
