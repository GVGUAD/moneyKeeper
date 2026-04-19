# Supabase Migration Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace SQLite + self-hosted auth with Supabase PostgreSQL + Supabase Auth JWT verification, keeping all business logic intact.

**Architecture:** Drop-in swap — `sqlx postgres` driver, adapt SQL syntax (`?` → `$N`, typed bindings), delete the local auth layer, verify Supabase-issued JWTs in middleware. DDD structure, repository interfaces, and `AuthUser(Uuid)` extractor shape are unchanged.

**Tech Stack:** sqlx 0.8 (postgres feature), jsonwebtoken 9 (HS256 + aud validation), `#[sqlx::test]` for all DB tests, Fly.io for deployment.

---

## Chunk 1: Dependencies, Migrations, Auth Removal

### Task 1: Cargo.toml + PostgreSQL Migrations

**Files:**
- Modify: `Cargo.toml`
- Delete: `src/infrastructure/migrations/001_users.sql`
- Delete: `src/infrastructure/migrations/002_accounts.sql`
- Delete: `src/infrastructure/migrations/003_transactions.sql`
- Delete: `src/infrastructure/migrations/004_external_id.sql`
- Delete: `src/infrastructure/migrations/005_monobank_connections.sql`
- Create: `src/infrastructure/migrations/0001_accounts.sql`
- Create: `src/infrastructure/migrations/0002_transactions.sql`
- Create: `src/infrastructure/migrations/0003_monobank.sql`

- [ ] **Step 1: Update `Cargo.toml`**

Add `postgres`, `uuid`, `chrono`, `rust_decimal` sqlx features. **Keep `sqlite`** — the repositories still use `SqlitePool` until Task 3. Remove auth deps (`argon2`, `rand`, `sha2`, `hex`). Add `sqlx` to dev-dependencies (required by `#[sqlx::test]`):

```toml
[package]
name = "moneykeeper"
version = "0.1.0"
edition = "2024"

[dependencies]
anyhow = "1"
thiserror = "2"
tokio = { version = "1", features = ["full"] }
async-trait = "0.1"
axum = { version = "0.8", features = ["macros"] }
sqlx = { version = "0.8", features = ["runtime-tokio", "sqlite", "postgres", "migrate", "uuid", "chrono", "rust_decimal"] }
jsonwebtoken = "9"
uuid = { version = "1", features = ["v4", "serde"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
chrono = { version = "0.4", features = ["serde"] }
rust_decimal = { version = "1", features = ["serde-with-str"] }
tower-http = { version = "0.6", features = ["cors", "trace"] }
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
dotenvy = "0.15"
reqwest = { version = "0.12", features = ["json", "rustls-tls"], default-features = false }

[dev-dependencies]
axum-test = "17"
sqlx = { version = "0.8", features = ["runtime-tokio", "postgres", "migrate", "uuid", "chrono", "rust_decimal"] }
```

Removed: `argon2`, `rand`, `sha2`, `hex`. Added `postgres`/`uuid`/`chrono`/`rust_decimal` to sqlx features (sqlite stays until Task 3). Added `sqlx` to dev-dependencies.

- [ ] **Step 2: Delete all 5 old migration files**

```bash
rm src/infrastructure/migrations/001_users.sql
rm src/infrastructure/migrations/002_accounts.sql
rm src/infrastructure/migrations/003_transactions.sql
rm src/infrastructure/migrations/004_external_id.sql
rm src/infrastructure/migrations/005_monobank_connections.sql
```

- [ ] **Step 3: Write `src/infrastructure/migrations/0001_accounts.sql`**

```sql
CREATE TABLE accounts (
    id          UUID        PRIMARY KEY NOT NULL,
    user_id     UUID        NOT NULL,
    name        TEXT        NOT NULL,
    account_type TEXT       NOT NULL,
    currency    TEXT        NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL,
    updated_at  TIMESTAMPTZ NOT NULL
);

-- account_type values: Cash | Bank | Savings | Loan | Investment | Binance
CREATE INDEX idx_accounts_user_id ON accounts(user_id);

CREATE TABLE savings_details (
    account_id       UUID    PRIMARY KEY NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    interest_rate    NUMERIC NOT NULL,
    compounding_period TEXT  NOT NULL
    -- compounding_period values: Daily | Monthly | Quarterly | Annually
);

CREATE TABLE loan_details (
    account_id    UUID    PRIMARY KEY NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    counterparty  TEXT    NOT NULL,
    direction     TEXT    NOT NULL,
    -- direction values: Borrowed | Lent
    interest_rate NUMERIC,
    due_date      DATE
);

CREATE TABLE investment_details (
    account_id UUID PRIMARY KEY NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    broker     TEXT
);

CREATE TABLE binance_details (
    account_id UUID PRIMARY KEY NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    label      TEXT
);
```

- [ ] **Step 4: Write `src/infrastructure/migrations/0002_transactions.sql`**

```sql
CREATE TABLE categories (
    id         UUID        PRIMARY KEY NOT NULL,
    user_id    UUID        NOT NULL,
    name       TEXT        NOT NULL,
    color      TEXT,
    created_at TIMESTAMPTZ NOT NULL
);

CREATE INDEX idx_categories_user_id ON categories(user_id);

CREATE TABLE transactions (
    id           UUID        PRIMARY KEY NOT NULL,
    account_id   UUID        NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    user_id      UUID        NOT NULL,
    amount       NUMERIC     NOT NULL,
    currency     TEXT        NOT NULL,
    kind         TEXT        NOT NULL,
    -- kind values: Income | Expense | Transfer | Buy | Sell | StakingReward
    category_id  UUID        REFERENCES categories(id) ON DELETE SET NULL,
    note         TEXT,
    external_id  TEXT,
    transacted_at TIMESTAMPTZ NOT NULL,
    created_at   TIMESTAMPTZ NOT NULL
);

-- Partial unique index for idempotent Monobank inserts
CREATE UNIQUE INDEX transactions_external_id_unique
    ON transactions (external_id)
    WHERE external_id IS NOT NULL;

CREATE INDEX idx_transactions_user_id       ON transactions(user_id);
CREATE INDEX idx_transactions_account_id    ON transactions(account_id);
CREATE INDEX idx_transactions_transacted_at ON transactions(transacted_at);
CREATE INDEX idx_transactions_category_id   ON transactions(category_id);

CREATE TABLE transfer_links (
    from_transaction_id UUID PRIMARY KEY NOT NULL REFERENCES transactions(id) ON DELETE CASCADE,
    to_transaction_id   UUID NOT NULL            REFERENCES transactions(id) ON DELETE CASCADE
);

CREATE TABLE trade_details (
    transaction_id UUID    PRIMARY KEY NOT NULL REFERENCES transactions(id) ON DELETE CASCADE,
    ticker         TEXT    NOT NULL,
    quantity       NUMERIC NOT NULL,
    price_per_unit NUMERIC,
    fee            NUMERIC
);
```

- [ ] **Step 5: Write `src/infrastructure/migrations/0003_monobank.sql`**

```sql
CREATE TABLE monobank_connections (
    id                  UUID    PRIMARY KEY NOT NULL,
    account_id          UUID    NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    user_id             UUID    NOT NULL,
    token               TEXT    NOT NULL,
    monobank_account_id TEXT    NOT NULL,
    sync_status         TEXT    NOT NULL DEFAULT 'pending',
    last_synced_at      BIGINT,
    created_at          BIGINT  NOT NULL
);

CREATE INDEX idx_monobank_connections_user_id    ON monobank_connections(user_id);
CREATE INDEX idx_monobank_connections_account_id ON monobank_connections(account_id);
CREATE UNIQUE INDEX idx_monobank_connections_monobank_account_id
    ON monobank_connections(monobank_account_id);
```

Note: `last_synced_at` and `created_at` stay `BIGINT` (Unix epoch). The repository stores them as `i64` — no change to that code path.

- [ ] **Step 6: Verify `cargo build` still compiles**

```bash
cargo build 2>&1 | head -20
```

Expected: compiles (old code still uses sqlite, but sqlx now has postgres feature too — both features can coexist until the driver swap in Task 3).

- [ ] **Step 7: Commit**

```bash
git add Cargo.toml Cargo.lock \
  src/infrastructure/migrations/0001_accounts.sql \
  src/infrastructure/migrations/0002_transactions.sql \
  src/infrastructure/migrations/0003_monobank.sql
git commit -m "feat: swap to postgres sqlx features and rewrite migrations for PostgreSQL"
```

---

### Task 2: Delete Auth Layer + Wire Supabase JWT

**Files:**
- Delete: `src/domain/user.rs`
- Delete: `src/application/auth.rs`
- Delete: `src/api/handlers/auth.rs`
- Delete: `src/infrastructure/user_repository.rs`
- Modify: `src/domain/mod.rs`
- Modify: `src/application/mod.rs`
- Modify: `src/infrastructure/mod.rs`
- Modify: `src/api/handlers/mod.rs`
- Modify: `src/api/jwt.rs`
- Modify: `src/api/middleware.rs`
- Modify: `src/api/state.rs`
- Modify: `src/api/routes.rs`
- Modify: `src/main.rs`

- [ ] **Step 1: Delete auth files**

```bash
rm src/domain/user.rs
rm src/application/auth.rs
rm src/api/handlers/auth.rs
rm src/infrastructure/user_repository.rs
```

- [ ] **Step 2: Remove `pub mod user;` from `src/domain/mod.rs`**

Find and remove the line `pub mod user;`. The remaining modules (account, transaction, category, monobank, error) stay unchanged.

- [ ] **Step 3: Remove `pub mod auth;` from `src/application/mod.rs`**

Find and remove the line `pub mod auth;`.

- [ ] **Step 4: Remove `pub mod user_repository;` from `src/infrastructure/mod.rs`**

Find and remove the line `pub mod user_repository;`.

- [ ] **Step 5: Remove `pub mod auth;` from `src/api/handlers/mod.rs`**

Find and remove the line `pub mod auth;`.

- [ ] **Step 6: Rewrite `src/api/jwt.rs`**

Replace the entire file. The old file had `create_token` (for issuing tokens) and a simple `Claims`. Now we only verify Supabase-issued tokens:

```rust
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode};
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct Claims {
    pub sub: String,           // Supabase user UUID
    pub email: Option<String>, // absent for some OAuth providers
    pub role: String,          // "authenticated"
    pub aud: Vec<String>,      // ["authenticated"] — Supabase encodes aud as an array
    pub exp: i64,
    pub iat: i64,
}

pub fn verify_token(token: &str, secret: &str) -> anyhow::Result<Claims> {
    let mut validation = Validation::new(Algorithm::HS256);
    validation.set_audience(&["authenticated"]);
    let data = decode::<Claims>(
        token,
        &DecodingKey::from_secret(secret.as_bytes()),
        &validation,
    )?;
    Ok(data.claims)
}
```

- [ ] **Step 7: Update `src/api/middleware.rs`**

Change `state.jwt_secret` → `state.supabase_jwt_secret`:

```rust
use axum::extract::{Request, State};
use axum::http::header::AUTHORIZATION;
use axum::middleware::Next;
use axum::response::Response;
use uuid::Uuid;

use crate::api::error::AppError;
use crate::api::jwt::verify_token;
use crate::api::state::AppState;
use crate::domain::error::DomainError;

#[derive(Clone, Debug)]
pub struct AuthUser(pub Uuid);

pub async fn auth_middleware(
    State(state): State<AppState>,
    mut req: Request,
    next: Next,
) -> Result<Response, AppError> {
    let header = req
        .headers()
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .ok_or(DomainError::Unauthorized)?;

    let claims = verify_token(header, &state.supabase_jwt_secret)
        .map_err(|_| DomainError::Unauthorized)?;

    let user_id = Uuid::parse_str(&claims.sub).map_err(|_| DomainError::Unauthorized)?;

    req.extensions_mut().insert(AuthUser(user_id));
    Ok(next.run(req).await)
}
```

- [ ] **Step 8: Rewrite `src/api/state.rs`**

Remove `AuthService`, rename `jwt_secret` → `supabase_jwt_secret`:

```rust
use std::sync::Arc;

use crate::application::accounts::AccountService;
use crate::application::categories::CategoryService;
use crate::application::monobank::MonobankService;
use crate::application::transactions::TransactionService;

#[derive(Clone)]
pub struct AppState {
    pub accounts: Arc<AccountService>,
    pub transactions: Arc<TransactionService>,
    pub categories: Arc<CategoryService>,
    pub monobank: Arc<MonobankService>,
    pub supabase_jwt_secret: String,
}
```

- [ ] **Step 9: Rewrite `src/api/routes.rs`**

Remove the `/auth/*` route group and add a public `GET /health` endpoint:

```rust
use axum::Router;
use axum::http::StatusCode;
use axum::middleware as axum_middleware;
use axum::routing::{delete, get, post, put};

use crate::api::handlers::{accounts, categories, monobank, transactions};
use crate::api::middleware::auth_middleware;
use crate::api::state::AppState;

pub fn router(state: AppState) -> Router {
    let protected = Router::new()
        .route(
            "/accounts",
            post(accounts::create_account).get(accounts::list_accounts),
        )
        .route(
            "/accounts/{id}",
            get(accounts::get_account)
                .put(accounts::update_account)
                .delete(accounts::delete_account),
        )
        .route("/accounts/{id}/balance", get(accounts::get_balance))
        .route(
            "/accounts/{id}/transactions",
            post(transactions::create_transaction).get(transactions::list_transactions),
        )
        .route("/transactions", get(transactions::list_all_transactions))
        .route(
            "/transactions/{id}",
            get(transactions::get_transaction).delete(transactions::delete_transaction),
        )
        .route(
            "/categories",
            post(categories::create_category).get(categories::list_categories),
        )
        .route(
            "/categories/{id}",
            put(categories::update_category).delete(categories::delete_category),
        )
        .route("/monobank/client-info", get(monobank::get_client_info))
        .route("/monobank/connect", post(monobank::connect))
        .route("/monobank/connections", get(monobank::list_connections))
        .route(
            "/monobank/connections/{id}",
            delete(monobank::delete_connection),
        )
        .layer(axum_middleware::from_fn_with_state(
            state.clone(),
            auth_middleware,
        ));

    Router::new()
        .route("/health", get(|| async { (StatusCode::OK, "ok") }))
        .merge(protected)
        .route("/monobank/webhook", post(monobank::webhook))
        .with_state(state)
}
```

- [ ] **Step 10: Rewrite `src/main.rs`**

Remove `AuthService` wiring, use `SUPABASE_JWT_SECRET`. Keep `SqlitePool` for now (pool swap happens in Task 3):

```rust
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
use moneykeeper::infrastructure::monobank_repository::SqliteMonobankRepository;
use moneykeeper::infrastructure::transaction_repository::SqliteTransactionRepository;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    let supabase_jwt_secret =
        std::env::var("SUPABASE_JWT_SECRET").expect("SUPABASE_JWT_SECRET must be set");
    let bind_addr = std::env::var("BIND_ADDR").unwrap_or_else(|_| "0.0.0.0:8080".to_string());
    let public_url =
        std::env::var("PUBLIC_URL").unwrap_or_else(|_| "http://localhost:3000".to_string());

    let pool = create_pool(&database_url).await?;

    let monobank_service = Arc::new(MonobankService::new(
        Arc::new(SqliteMonobankRepository::new(pool.clone())),
        Arc::new(SqliteTransactionRepository::new(pool.clone())),
        Arc::new(ReqwestMonobankClient::new()),
        public_url,
    ));

    let state = AppState {
        accounts: Arc::new(AccountService::new(Arc::new(SqliteAccountRepository::new(
            pool.clone(),
        )))),
        transactions: Arc::new(TransactionService::new(Arc::new(
            SqliteTransactionRepository::new(pool.clone()),
        ))),
        categories: Arc::new(CategoryService::new(Arc::new(
            SqliteCategoryRepository::new(pool.clone()),
        ))),
        monobank: monobank_service.clone(),
        supabase_jwt_secret,
    };

    monobank_service.restart_incomplete_syncs().await;

    let router = api::routes::router(state);
    let listener = tokio::net::TcpListener::bind(&bind_addr).await?;
    tracing::info!("listening on {bind_addr}");
    axum::serve(listener, router).await?;
    Ok(())
}
```

- [ ] **Step 11: Verify it compiles**

```bash
cargo build 2>&1 | head -30
```

Expected: compiles cleanly. The app still uses SQLite pool internally — that's intentional; the driver swap is Task 3.

- [ ] **Step 12: Commit**

```bash
git add src/domain/mod.rs src/application/mod.rs src/infrastructure/mod.rs \
  src/api/handlers/mod.rs src/api/jwt.rs src/api/middleware.rs \
  src/api/state.rs src/api/routes.rs src/main.rs
git commit -m "feat: remove auth layer and wire Supabase JWT verification"
```

---

## Chunk 2: PostgreSQL DB Layer

### Task 3: Port DB Layer to PostgreSQL

All 4 repositories + `db.rs` + `main.rs` must be updated together — `main.rs` passes the pool to all repos, so all pool types must agree. After all sub-steps, run one `cargo build`.

**Files:**
- Modify: `src/infrastructure/db.rs`
- Modify: `src/infrastructure/account_repository.rs`
- Modify: `src/infrastructure/transaction_repository.rs`
- Modify: `src/infrastructure/category_repository.rs`
- Modify: `src/infrastructure/monobank_repository.rs`
- Modify: `src/main.rs`

- [ ] **Step 1: Rewrite `src/infrastructure/db.rs`**

```rust
use sqlx::postgres::{PgPool, PgPoolOptions};

pub async fn create_pool(database_url: &str) -> anyhow::Result<PgPool> {
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(database_url)
        .await?;
    sqlx::migrate!("src/infrastructure/migrations")
        .run(&pool)
        .await?;
    Ok(pool)
}
```

- [ ] **Step 2: Rewrite `src/infrastructure/account_repository.rs`**

Key changes: `SqlitePool` → `PgPool`, typed row fields (`Uuid`, `DateTime<Utc>`, `Decimal`, `NaiveDate`), `?` → `$N` in SQL, remove manual string conversion for UUIDs and timestamps.

```rust
use anyhow::Context;
use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::Decimal;
use sqlx::PgPool;
use std::str::FromStr;
use uuid::Uuid;

use crate::domain::account::{
    Account, AccountDetails, AccountRepository, AccountType, BinanceDetails, CompoundingPeriod,
    InvestmentDetails, LoanDetails, LoanDirection, SavingsDetails,
};
use crate::domain::transaction::TransactionKind;

pub struct SqliteAccountRepository {
    pool: PgPool,
}

impl SqliteAccountRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[derive(sqlx::FromRow)]
struct AccountRow {
    id: Uuid,
    user_id: Uuid,
    name: String,
    account_type: String,
    currency: String,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

fn row_to_account(r: AccountRow) -> anyhow::Result<Account> {
    Ok(Account {
        id: r.id,
        user_id: r.user_id,
        name: r.name,
        account_type: AccountType::from_str(&r.account_type)?,
        currency: r.currency,
        created_at: r.created_at,
        updated_at: r.updated_at,
    })
}

#[async_trait::async_trait]
impl AccountRepository for SqliteAccountRepository {
    async fn create(&self, account: &Account, details: &AccountDetails) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO accounts (id, user_id, name, account_type, currency, created_at, updated_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(account.id)
        .bind(account.user_id)
        .bind(&account.name)
        .bind(account.account_type.as_str())
        .bind(&account.currency)
        .bind(account.created_at)
        .bind(account.updated_at)
        .execute(&self.pool)
        .await
        .context("insert account")?;

        self.insert_details(account.id, details).await
    }

    async fn find_by_id(
        &self,
        id: Uuid,
        user_id: Uuid,
    ) -> anyhow::Result<Option<(Account, AccountDetails)>> {
        let row = sqlx::query_as::<_, AccountRow>(
            "SELECT * FROM accounts WHERE id = $1 AND user_id = $2",
        )
        .bind(id)
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await?;

        match row {
            None => Ok(None),
            Some(r) => {
                let account = row_to_account(r)?;
                let details = self
                    .fetch_details(account.id, &account.account_type)
                    .await?;
                Ok(Some((account, details)))
            }
        }
    }

    async fn list_by_user(&self, user_id: Uuid) -> anyhow::Result<Vec<(Account, AccountDetails)>> {
        let rows = sqlx::query_as::<_, AccountRow>(
            "SELECT * FROM accounts WHERE user_id = $1 ORDER BY created_at",
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await?;

        let mut result = Vec::with_capacity(rows.len());
        for row in rows {
            let account = row_to_account(row)?;
            let details = self
                .fetch_details(account.id, &account.account_type)
                .await?;
            result.push((account, details));
        }
        Ok(result)
    }

    async fn update(&self, account: &Account, details: &AccountDetails) -> anyhow::Result<()> {
        sqlx::query(
            "UPDATE accounts SET name = $1, currency = $2, updated_at = $3 \
             WHERE id = $4 AND user_id = $5",
        )
        .bind(&account.name)
        .bind(&account.currency)
        .bind(account.updated_at)
        .bind(account.id)
        .bind(account.user_id)
        .execute(&self.pool)
        .await?;

        self.delete_details(account.id, &account.account_type)
            .await?;
        self.insert_details(account.id, details).await
    }

    async fn delete(&self, id: Uuid, user_id: Uuid) -> anyhow::Result<()> {
        sqlx::query("DELETE FROM accounts WHERE id = $1 AND user_id = $2")
            .bind(id)
            .bind(user_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn compute_balance(&self, account_id: Uuid, user_id: Uuid) -> anyhow::Result<Decimal> {
        let rows: Vec<(Decimal, String)> = sqlx::query_as(
            "SELECT amount, kind FROM transactions WHERE account_id = $1 AND user_id = $2",
        )
        .bind(account_id)
        .bind(user_id)
        .fetch_all(&self.pool)
        .await?;

        let mut balance = Decimal::ZERO;
        for (amount, kind_str) in rows {
            let kind = TransactionKind::from_str(&kind_str)?;
            if kind.affects_balance_positively() {
                balance += amount;
            } else {
                balance -= amount;
            }
        }
        Ok(balance)
    }
}

impl SqliteAccountRepository {
    async fn insert_details(&self, id: Uuid, details: &AccountDetails) -> anyhow::Result<()> {
        match details {
            AccountDetails::Savings(d) => {
                sqlx::query(
                    "INSERT INTO savings_details (account_id, interest_rate, compounding_period) \
                     VALUES ($1, $2, $3)",
                )
                .bind(id)
                .bind(d.interest_rate)
                .bind(d.compounding_period.as_str())
                .execute(&self.pool)
                .await?;
            }
            AccountDetails::Loan(d) => {
                sqlx::query(
                    "INSERT INTO loan_details \
                     (account_id, counterparty, direction, interest_rate, due_date) \
                     VALUES ($1, $2, $3, $4, $5)",
                )
                .bind(id)
                .bind(&d.counterparty)
                .bind(d.direction.as_str())
                .bind(d.interest_rate)
                .bind(d.due_date)
                .execute(&self.pool)
                .await?;
            }
            AccountDetails::Investment(d) => {
                sqlx::query(
                    "INSERT INTO investment_details (account_id, broker) VALUES ($1, $2)",
                )
                .bind(id)
                .bind(&d.broker)
                .execute(&self.pool)
                .await?;
            }
            AccountDetails::Binance(d) => {
                sqlx::query(
                    "INSERT INTO binance_details (account_id, label) VALUES ($1, $2)",
                )
                .bind(id)
                .bind(&d.label)
                .execute(&self.pool)
                .await?;
            }
            AccountDetails::None => {}
        }
        Ok(())
    }

    async fn delete_details(&self, id: Uuid, account_type: &AccountType) -> anyhow::Result<()> {
        match account_type {
            AccountType::Savings => {
                sqlx::query("DELETE FROM savings_details WHERE account_id = $1")
                    .bind(id)
                    .execute(&self.pool)
                    .await?;
            }
            AccountType::Loan => {
                sqlx::query("DELETE FROM loan_details WHERE account_id = $1")
                    .bind(id)
                    .execute(&self.pool)
                    .await?;
            }
            AccountType::Investment => {
                sqlx::query("DELETE FROM investment_details WHERE account_id = $1")
                    .bind(id)
                    .execute(&self.pool)
                    .await?;
            }
            AccountType::Binance => {
                sqlx::query("DELETE FROM binance_details WHERE account_id = $1")
                    .bind(id)
                    .execute(&self.pool)
                    .await?;
            }
            _ => {}
        }
        Ok(())
    }

    async fn fetch_details(
        &self,
        id: Uuid,
        account_type: &AccountType,
    ) -> anyhow::Result<AccountDetails> {
        match account_type {
            AccountType::Savings => {
                let row: Option<(Decimal, String)> = sqlx::query_as(
                    "SELECT interest_rate, compounding_period \
                     FROM savings_details WHERE account_id = $1",
                )
                .bind(id)
                .fetch_optional(&self.pool)
                .await?;
                match row {
                    Some((rate, period)) => Ok(AccountDetails::Savings(SavingsDetails {
                        account_id: id,
                        interest_rate: rate,
                        compounding_period: CompoundingPeriod::from_str(&period)?,
                    })),
                    None => Ok(AccountDetails::None),
                }
            }
            AccountType::Loan => {
                let row: Option<(String, String, Option<Decimal>, Option<NaiveDate>)> =
                    sqlx::query_as(
                        "SELECT counterparty, direction, interest_rate, due_date \
                         FROM loan_details WHERE account_id = $1",
                    )
                    .bind(id)
                    .fetch_optional(&self.pool)
                    .await?;
                match row {
                    Some((counterparty, direction, rate, due_date)) => {
                        Ok(AccountDetails::Loan(LoanDetails {
                            account_id: id,
                            counterparty,
                            direction: LoanDirection::from_str(&direction)?,
                            interest_rate: rate,
                            due_date,
                        }))
                    }
                    None => Ok(AccountDetails::None),
                }
            }
            AccountType::Investment => {
                let row: Option<(Option<String>,)> =
                    sqlx::query_as("SELECT broker FROM investment_details WHERE account_id = $1")
                        .bind(id)
                        .fetch_optional(&self.pool)
                        .await?;
                match row {
                    Some((broker,)) => Ok(AccountDetails::Investment(InvestmentDetails {
                        account_id: id,
                        broker,
                    })),
                    None => Ok(AccountDetails::None),
                }
            }
            AccountType::Binance => {
                let row: Option<(Option<String>,)> =
                    sqlx::query_as("SELECT label FROM binance_details WHERE account_id = $1")
                        .bind(id)
                        .fetch_optional(&self.pool)
                        .await?;
                match row {
                    Some((label,)) => Ok(AccountDetails::Binance(BinanceDetails {
                        account_id: id,
                        label,
                    })),
                    None => Ok(AccountDetails::None),
                }
            }
            _ => Ok(AccountDetails::None),
        }
    }
}
```

- [ ] **Step 3: Rewrite `src/infrastructure/transaction_repository.rs`**

Key changes: typed row fields, `?` → `$N`, fix dynamic SQL in `list()` (numbered params), `INSERT OR IGNORE` → `ON CONFLICT ... DO NOTHING`, bind `Decimal`/`Uuid`/`DateTime<Utc>` directly.

```rust
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use sqlx::PgPool;
use std::str::FromStr;
use uuid::Uuid;

use crate::domain::transaction::{
    TradeDetails, Transaction, TransactionDetails, TransactionKind, TransactionListParams,
    TransactionRepository, TransferLink,
};

pub struct SqliteTransactionRepository {
    pool: PgPool,
}

impl SqliteTransactionRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[derive(sqlx::FromRow)]
struct TxRow {
    id: Uuid,
    account_id: Uuid,
    user_id: Uuid,
    amount: Decimal,
    currency: String,
    kind: String,
    category_id: Option<Uuid>,
    note: Option<String>,
    external_id: Option<String>,
    transacted_at: DateTime<Utc>,
    created_at: DateTime<Utc>,
}

fn row_to_tx(r: TxRow) -> anyhow::Result<Transaction> {
    Ok(Transaction {
        id: r.id,
        account_id: r.account_id,
        user_id: r.user_id,
        amount: r.amount,
        currency: r.currency,
        kind: TransactionKind::from_str(&r.kind)?,
        category_id: r.category_id,
        note: r.note,
        external_id: r.external_id,
        transacted_at: r.transacted_at,
        created_at: r.created_at,
    })
}

#[async_trait::async_trait]
impl TransactionRepository for SqliteTransactionRepository {
    async fn create(&self, tx: &Transaction, details: &TransactionDetails) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO transactions \
             (id, account_id, user_id, amount, currency, kind, category_id, note, external_id, \
              transacted_at, created_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
        )
        .bind(tx.id)
        .bind(tx.account_id)
        .bind(tx.user_id)
        .bind(tx.amount)
        .bind(&tx.currency)
        .bind(tx.kind.as_str())
        .bind(tx.category_id)
        .bind(&tx.note)
        .bind(&tx.external_id)
        .bind(tx.transacted_at)
        .bind(tx.created_at)
        .execute(&self.pool)
        .await?;

        match details {
            TransactionDetails::Transfer(link) => {
                sqlx::query(
                    "INSERT INTO transfer_links (from_transaction_id, to_transaction_id) \
                     VALUES ($1, $2)",
                )
                .bind(link.from_transaction_id)
                .bind(link.to_transaction_id)
                .execute(&self.pool)
                .await?;
            }
            TransactionDetails::Trade(trade) => {
                sqlx::query(
                    "INSERT INTO trade_details \
                     (transaction_id, ticker, quantity, price_per_unit, fee) \
                     VALUES ($1, $2, $3, $4, $5)",
                )
                .bind(trade.transaction_id)
                .bind(&trade.ticker)
                .bind(trade.quantity)
                .bind(trade.price_per_unit)
                .bind(trade.fee)
                .execute(&self.pool)
                .await?;
            }
            TransactionDetails::None => {}
        }
        Ok(())
    }

    async fn find_by_id(
        &self,
        id: Uuid,
        user_id: Uuid,
    ) -> anyhow::Result<Option<(Transaction, TransactionDetails)>> {
        let row = sqlx::query_as::<_, TxRow>(
            "SELECT * FROM transactions WHERE id = $1 AND user_id = $2",
        )
        .bind(id)
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await?;

        match row {
            None => Ok(None),
            Some(r) => {
                let tx = row_to_tx(r)?;
                let details = self.fetch_details(&tx).await?;
                Ok(Some((tx, details)))
            }
        }
    }

    async fn list(
        &self,
        params: &TransactionListParams,
    ) -> anyhow::Result<Vec<(Transaction, TransactionDetails)>> {
        // PostgreSQL requires numbered placeholders $1, $2, ... — build them dynamically.
        let mut conditions = vec!["user_id = $1".to_string()];
        let mut param_count = 1usize;

        if params.account_id.is_some() {
            param_count += 1;
            conditions.push(format!("account_id = ${param_count}"));
        }
        if params.kind.is_some() {
            param_count += 1;
            conditions.push(format!("kind = ${param_count}"));
        }
        if params.category_id.is_some() {
            param_count += 1;
            conditions.push(format!("category_id = ${param_count}"));
        }
        if params.from.is_some() {
            param_count += 1;
            conditions.push(format!("transacted_at >= ${param_count}"));
        }
        if params.to.is_some() {
            param_count += 1;
            conditions.push(format!("transacted_at <= ${param_count}"));
        }
        param_count += 1;
        let limit_param = param_count;
        param_count += 1;
        let offset_param = param_count;

        let sql = format!(
            "SELECT * FROM transactions WHERE {} \
             ORDER BY transacted_at DESC LIMIT ${limit_param} OFFSET ${offset_param}",
            conditions.join(" AND ")
        );

        let mut q = sqlx::query_as::<_, TxRow>(&sql).bind(params.user_id);
        if let Some(acc) = params.account_id {
            q = q.bind(acc);
        }
        if let Some(k) = &params.kind {
            q = q.bind(k.as_str());
        }
        if let Some(cat) = params.category_id {
            q = q.bind(cat);
        }
        if let Some(from) = params.from {
            q = q.bind(from);
        }
        if let Some(to) = params.to {
            q = q.bind(to);
        }
        let rows = q
            .bind(params.limit)
            .bind(params.offset)
            .fetch_all(&self.pool)
            .await?;

        let mut result = Vec::with_capacity(rows.len());
        for row in rows {
            let tx = row_to_tx(row)?;
            let details = self.fetch_details(&tx).await?;
            result.push((tx, details));
        }
        Ok(result)
    }

    async fn update(&self, tx: &Transaction, details: &TransactionDetails) -> anyhow::Result<()> {
        sqlx::query(
            "UPDATE transactions \
             SET amount = $1, currency = $2, kind = $3, category_id = $4, note = $5, \
                 transacted_at = $6 \
             WHERE id = $7 AND user_id = $8",
        )
        .bind(tx.amount)
        .bind(&tx.currency)
        .bind(tx.kind.as_str())
        .bind(tx.category_id)
        .bind(&tx.note)
        .bind(tx.transacted_at)
        .bind(tx.id)
        .bind(tx.user_id)
        .execute(&self.pool)
        .await?;

        sqlx::query("DELETE FROM trade_details WHERE transaction_id = $1")
            .bind(tx.id)
            .execute(&self.pool)
            .await?;
        sqlx::query("DELETE FROM transfer_links WHERE from_transaction_id = $1")
            .bind(tx.id)
            .execute(&self.pool)
            .await?;

        if let TransactionDetails::Trade(trade) = details {
            sqlx::query(
                "INSERT INTO trade_details \
                 (transaction_id, ticker, quantity, price_per_unit, fee) \
                 VALUES ($1, $2, $3, $4, $5)",
            )
            .bind(trade.transaction_id)
            .bind(&trade.ticker)
            .bind(trade.quantity)
            .bind(trade.price_per_unit)
            .bind(trade.fee)
            .execute(&self.pool)
            .await?;
        }
        Ok(())
    }

    async fn delete(&self, id: Uuid, user_id: Uuid) -> anyhow::Result<()> {
        sqlx::query("DELETE FROM transactions WHERE id = $1 AND user_id = $2")
            .bind(id)
            .bind(user_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn create_idempotent(&self, tx: &Transaction) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO transactions \
             (id, account_id, user_id, amount, currency, kind, category_id, note, external_id, \
              transacted_at, created_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11) \
             ON CONFLICT (external_id) WHERE external_id IS NOT NULL DO NOTHING",
        )
        .bind(tx.id)
        .bind(tx.account_id)
        .bind(tx.user_id)
        .bind(tx.amount)
        .bind(&tx.currency)
        .bind(tx.kind.as_str())
        .bind(tx.category_id)
        .bind(&tx.note)
        .bind(&tx.external_id)
        .bind(tx.transacted_at)
        .bind(tx.created_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

impl SqliteTransactionRepository {
    async fn fetch_details(&self, tx: &Transaction) -> anyhow::Result<TransactionDetails> {
        match tx.kind {
            TransactionKind::Transfer => {
                let row: Option<(Uuid,)> = sqlx::query_as(
                    "SELECT to_transaction_id FROM transfer_links \
                     WHERE from_transaction_id = $1",
                )
                .bind(tx.id)
                .fetch_optional(&self.pool)
                .await?;
                if let Some((to_id,)) = row {
                    return Ok(TransactionDetails::Transfer(TransferLink {
                        from_transaction_id: tx.id,
                        to_transaction_id: to_id,
                    }));
                }
                Ok(TransactionDetails::None)
            }
            TransactionKind::Buy | TransactionKind::Sell | TransactionKind::StakingReward => {
                let row: Option<(String, Decimal, Option<Decimal>, Option<Decimal>)> =
                    sqlx::query_as(
                        "SELECT ticker, quantity, price_per_unit, fee \
                         FROM trade_details WHERE transaction_id = $1",
                    )
                    .bind(tx.id)
                    .fetch_optional(&self.pool)
                    .await?;
                if let Some((ticker, quantity, price, fee)) = row {
                    return Ok(TransactionDetails::Trade(TradeDetails {
                        transaction_id: tx.id,
                        ticker,
                        quantity,
                        price_per_unit: price,
                        fee,
                    }));
                }
                Ok(TransactionDetails::None)
            }
            _ => Ok(TransactionDetails::None),
        }
    }
}
```

- [ ] **Step 4: Rewrite `src/infrastructure/category_repository.rs`**

`row_to_category` is now infallible (all fields are natively typed):

```rust
use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use crate::domain::category::{Category, CategoryRepository};

pub struct SqliteCategoryRepository {
    pool: PgPool,
}

impl SqliteCategoryRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[derive(sqlx::FromRow)]
struct CategoryRow {
    id: Uuid,
    user_id: Uuid,
    name: String,
    color: Option<String>,
    created_at: DateTime<Utc>,
}

fn row_to_category(r: CategoryRow) -> Category {
    Category {
        id: r.id,
        user_id: r.user_id,
        name: r.name,
        color: r.color,
        created_at: r.created_at,
    }
}

#[async_trait::async_trait]
impl CategoryRepository for SqliteCategoryRepository {
    async fn create(&self, c: &Category) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO categories (id, user_id, name, color, created_at) \
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(c.id)
        .bind(c.user_id)
        .bind(&c.name)
        .bind(&c.color)
        .bind(c.created_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn list_by_user(&self, user_id: Uuid) -> anyhow::Result<Vec<Category>> {
        let rows = sqlx::query_as::<_, CategoryRow>(
            "SELECT * FROM categories WHERE user_id = $1 ORDER BY name",
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(row_to_category).collect())
    }

    async fn find_by_id(&self, id: Uuid, user_id: Uuid) -> anyhow::Result<Option<Category>> {
        let row = sqlx::query_as::<_, CategoryRow>(
            "SELECT * FROM categories WHERE id = $1 AND user_id = $2",
        )
        .bind(id)
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(row_to_category))
    }

    async fn update(&self, c: &Category) -> anyhow::Result<()> {
        sqlx::query(
            "UPDATE categories SET name = $1, color = $2 WHERE id = $3 AND user_id = $4",
        )
        .bind(&c.name)
        .bind(&c.color)
        .bind(c.id)
        .bind(c.user_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn delete(&self, id: Uuid, user_id: Uuid) -> anyhow::Result<()> {
        sqlx::query("DELETE FROM categories WHERE id = $1 AND user_id = $2")
            .bind(id)
            .bind(user_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}
```

- [ ] **Step 5: Rewrite `src/infrastructure/monobank_repository.rs`**

UUID fields go native; timestamps stay `BIGINT`/`i64` (no change to that logic). Drop `use std::str::FromStr` if unused elsewhere in the file, but keep for `SyncStatus::from_str`.

```rust
use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use crate::domain::monobank::{MonobankConnection, MonobankConnectionRepository, SyncStatus};

pub struct SqliteMonobankRepository {
    pool: PgPool,
}

impl SqliteMonobankRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[derive(sqlx::FromRow)]
struct ConnectionRow {
    id: Uuid,
    account_id: Uuid,
    user_id: Uuid,
    token: String,
    monobank_account_id: String,
    sync_status: String,
    last_synced_at: Option<i64>,
    created_at: i64,
}

fn row_to_conn(r: ConnectionRow) -> anyhow::Result<MonobankConnection> {
    Ok(MonobankConnection {
        id: r.id,
        account_id: r.account_id,
        user_id: r.user_id,
        token: r.token,
        monobank_account_id: r.monobank_account_id,
        sync_status: SyncStatus::from_str(&r.sync_status)?,
        last_synced_at: r.last_synced_at.and_then(|ts| DateTime::from_timestamp(ts, 0)),
        created_at: DateTime::from_timestamp(r.created_at, 0)
            .ok_or_else(|| anyhow::anyhow!("invalid created_at timestamp"))?,
    })
}

#[async_trait::async_trait]
impl MonobankConnectionRepository for SqliteMonobankRepository {
    async fn create(&self, conn: &MonobankConnection) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO monobank_connections \
             (id, account_id, user_id, token, monobank_account_id, sync_status, \
              last_synced_at, created_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(conn.id)
        .bind(conn.account_id)
        .bind(conn.user_id)
        .bind(&conn.token)
        .bind(&conn.monobank_account_id)
        .bind(conn.sync_status.as_str())
        .bind(conn.last_synced_at.map(|dt| dt.timestamp()))
        .bind(conn.created_at.timestamp())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn find_by_id(
        &self,
        id: Uuid,
        user_id: Uuid,
    ) -> anyhow::Result<Option<MonobankConnection>> {
        let row = sqlx::query_as::<_, ConnectionRow>(
            "SELECT * FROM monobank_connections WHERE id = $1 AND user_id = $2",
        )
        .bind(id)
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(row_to_conn).transpose()
    }

    async fn find_by_monobank_account_id(
        &self,
        monobank_account_id: &str,
    ) -> anyhow::Result<Option<MonobankConnection>> {
        let row = sqlx::query_as::<_, ConnectionRow>(
            "SELECT * FROM monobank_connections WHERE monobank_account_id = $1",
        )
        .bind(monobank_account_id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(row_to_conn).transpose()
    }

    async fn list_by_user(&self, user_id: Uuid) -> anyhow::Result<Vec<MonobankConnection>> {
        let rows = sqlx::query_as::<_, ConnectionRow>(
            "SELECT * FROM monobank_connections WHERE user_id = $1 ORDER BY created_at",
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(row_to_conn).collect()
    }

    async fn list_incomplete(&self) -> anyhow::Result<Vec<MonobankConnection>> {
        let rows = sqlx::query_as::<_, ConnectionRow>(
            "SELECT * FROM monobank_connections \
             WHERE sync_status IN ('pending', 'syncing') ORDER BY created_at",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(row_to_conn).collect()
    }

    async fn update_status(
        &self,
        id: Uuid,
        status: SyncStatus,
        last_synced_at: Option<DateTime<Utc>>,
    ) -> anyhow::Result<()> {
        sqlx::query(
            "UPDATE monobank_connections \
             SET sync_status = $1, last_synced_at = $2 WHERE id = $3",
        )
        .bind(status.as_str())
        .bind(last_synced_at.map(|dt| dt.timestamp()))
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn delete(&self, id: Uuid, user_id: Uuid) -> anyhow::Result<()> {
        sqlx::query(
            "DELETE FROM monobank_connections WHERE id = $1 AND user_id = $2",
        )
        .bind(id)
        .bind(user_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}
```

- [ ] **Step 6: Update `src/main.rs` — swap to `PgPool` imports**

Change all `Sqlite*` import paths to use the same struct names but now backed by `PgPool`. Since the struct names are unchanged (e.g., `SqliteAccountRepository` still exists, now wrapping `PgPool`), **only the `db` import and the pool type need to change**. The `create_pool` function now returns `PgPool`, so `pool.clone()` propagates the right type throughout. No other changes needed in `main.rs` beyond the already-made `SUPABASE_JWT_SECRET` changes in Task 2.

Verify no remaining references to `SqliteUserRepository` or `AuthService`:

```bash
grep -n "SqliteUser\|AuthService\|sqlite" src/main.rs
```

Expected: no matches.

- [ ] **Step 7: Remove `sqlite` from sqlx features in `Cargo.toml`**

Now that all repositories use `PgPool`, the `sqlite` feature is no longer needed. Update the sqlx line in `[dependencies]`:

```toml
sqlx = { version = "0.8", features = ["runtime-tokio", "postgres", "migrate", "uuid", "chrono", "rust_decimal"] }
```

- [ ] **Step 9: Verify the full build compiles**

```bash
cargo build 2>&1 | head -40
```

Expected: compiles cleanly. If there are errors, fix them before continuing.

- [ ] **Step 10: Commit**

```bash
git add Cargo.toml Cargo.lock \
  src/infrastructure/db.rs \
  src/infrastructure/account_repository.rs \
  src/infrastructure/transaction_repository.rs \
  src/infrastructure/category_repository.rs \
  src/infrastructure/monobank_repository.rs \
  src/main.rs
git commit -m "feat: port all repositories and db layer to PostgreSQL"
```

---

## Chunk 3: Tests

### Task 4: Update Repository Unit Tests

All repository unit tests currently create in-memory SQLite pools and insert a `User` row to satisfy FK constraints. Replace with `#[sqlx::test]` — it creates a real isolated Postgres database per test and runs migrations automatically. No user FK exists in the new schema, so `user_id = Uuid::new_v4()` suffices.

**`#[sqlx::test]` requirements:**
- sqlx in `[dev-dependencies]` ✓ (added in Task 1)
- `DATABASE_URL` env var pointing to a Postgres instance at test time
- `migrations = "src/infrastructure/migrations"` attribute (path relative to crate root)

**Files:**
- Modify: `src/infrastructure/account_repository.rs` (test section only)
- Modify: `src/infrastructure/transaction_repository.rs` (test section only)
- Modify: `src/infrastructure/category_repository.rs` (test section only)
- Modify: `src/infrastructure/monobank_repository.rs` (test section only)

- [ ] **Step 1: Replace tests in `src/infrastructure/account_repository.rs`**

Replace the entire `#[cfg(test)] mod tests { ... }` block:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::PgPool;

    #[sqlx::test(migrations = "src/infrastructure/migrations")]
    async fn create_and_find_cash_account(pool: PgPool) {
        let repo = SqliteAccountRepository::new(pool);
        let user_id = Uuid::new_v4();
        let account =
            Account::new(user_id, "Wallet".to_string(), AccountType::Cash, "USD".to_string());
        repo.create(&account, &AccountDetails::None).await.unwrap();
        let (found, details) = repo.find_by_id(account.id, user_id).await.unwrap().unwrap();
        assert_eq!(found.name, "Wallet");
        assert!(matches!(details, AccountDetails::None));
    }

    #[sqlx::test(migrations = "src/infrastructure/migrations")]
    async fn create_and_find_savings_account(pool: PgPool) {
        let repo = SqliteAccountRepository::new(pool);
        let user_id = Uuid::new_v4();
        let account = Account::new(
            user_id,
            "Savings".to_string(),
            AccountType::Savings,
            "USD".to_string(),
        );
        let details = AccountDetails::Savings(SavingsDetails {
            account_id: account.id,
            interest_rate: Decimal::new(5, 2),
            compounding_period: CompoundingPeriod::Monthly,
        });
        repo.create(&account, &details).await.unwrap();
        let (_, found_details) = repo.find_by_id(account.id, user_id).await.unwrap().unwrap();
        if let AccountDetails::Savings(s) = found_details {
            assert_eq!(s.compounding_period, CompoundingPeriod::Monthly);
        } else {
            panic!("expected savings details");
        }
    }

    #[sqlx::test(migrations = "src/infrastructure/migrations")]
    async fn cannot_find_other_users_account(pool: PgPool) {
        let repo = SqliteAccountRepository::new(pool);
        let user_id = Uuid::new_v4();
        let account =
            Account::new(user_id, "Wallet".to_string(), AccountType::Cash, "USD".to_string());
        repo.create(&account, &AccountDetails::None).await.unwrap();
        let other_user_id = Uuid::new_v4();
        let result = repo.find_by_id(account.id, other_user_id).await.unwrap();
        assert!(result.is_none());
    }
}
```

- [ ] **Step 2: Replace tests in `src/infrastructure/transaction_repository.rs`**

Replace the entire test block. The `setup` helper now takes the pool (injected by `#[sqlx::test]`) and no longer needs a User row:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::account::{Account, AccountDetails, AccountRepository, AccountType};
    use crate::infrastructure::account_repository::SqliteAccountRepository;
    use sqlx::PgPool;

    async fn setup(pool: PgPool) -> (PgPool, Uuid, Uuid) {
        let user_id = Uuid::new_v4();
        let account =
            Account::new(user_id, "Cash".to_string(), AccountType::Cash, "USD".to_string());
        let account_id = account.id;
        SqliteAccountRepository::new(pool.clone())
            .create(&account, &AccountDetails::None)
            .await
            .unwrap();
        (pool, user_id, account_id)
    }

    #[sqlx::test(migrations = "src/infrastructure/migrations")]
    async fn create_and_find_income_transaction(pool: PgPool) {
        let (pool, user_id, account_id) = setup(pool).await;
        let repo = SqliteTransactionRepository::new(pool);
        let tx = Transaction::new(
            account_id,
            user_id,
            Decimal::new(100, 0),
            "USD".to_string(),
            TransactionKind::Income,
            None,
            Some("salary".to_string()),
            Utc::now(),
        );
        repo.create(&tx, &TransactionDetails::None).await.unwrap();
        let (found, _) = repo.find_by_id(tx.id, user_id).await.unwrap().unwrap();
        assert_eq!(found.amount, Decimal::new(100, 0));
        assert_eq!(found.note, Some("salary".to_string()));
    }

    #[sqlx::test(migrations = "src/infrastructure/migrations")]
    async fn list_transactions_filtered_by_kind(pool: PgPool) {
        let (pool, user_id, account_id) = setup(pool).await;
        let repo = SqliteTransactionRepository::new(pool);
        let income = Transaction::new(
            account_id, user_id, Decimal::new(100, 0), "USD".to_string(),
            TransactionKind::Income, None, None, Utc::now(),
        );
        let expense = Transaction::new(
            account_id, user_id, Decimal::new(50, 0), "USD".to_string(),
            TransactionKind::Expense, None, None, Utc::now(),
        );
        repo.create(&income, &TransactionDetails::None).await.unwrap();
        repo.create(&expense, &TransactionDetails::None).await.unwrap();
        let params = TransactionListParams {
            account_id: Some(account_id),
            user_id,
            kind: Some(TransactionKind::Income),
            category_id: None,
            from: None,
            to: None,
            limit: 10,
            offset: 0,
        };
        let results = repo.list(&params).await.unwrap();
        assert_eq!(results.len(), 1);
        assert!(matches!(results[0].0.kind, TransactionKind::Income));
    }
}
```

- [ ] **Step 3: Replace tests in `src/infrastructure/category_repository.rs`**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::PgPool;

    #[sqlx::test(migrations = "src/infrastructure/migrations")]
    async fn create_list_delete_category(pool: PgPool) {
        let repo = SqliteCategoryRepository::new(pool);
        let user_id = Uuid::new_v4();
        let cat = Category::new(user_id, "Food".to_string(), Some("#ff0000".to_string()));
        repo.create(&cat).await.unwrap();
        let list = repo.list_by_user(user_id).await.unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "Food");
        repo.delete(cat.id, user_id).await.unwrap();
        let list = repo.list_by_user(user_id).await.unwrap();
        assert!(list.is_empty());
    }

    #[sqlx::test(migrations = "src/infrastructure/migrations")]
    async fn update_category_name(pool: PgPool) {
        let repo = SqliteCategoryRepository::new(pool);
        let user_id = Uuid::new_v4();
        let mut cat = Category::new(user_id, "Food".to_string(), None);
        repo.create(&cat).await.unwrap();
        cat.name = "Groceries".to_string();
        repo.update(&cat).await.unwrap();
        let found = repo.find_by_id(cat.id, user_id).await.unwrap().unwrap();
        assert_eq!(found.name, "Groceries");
    }
}
```

- [ ] **Step 4: Replace tests in `src/infrastructure/monobank_repository.rs`**

The `monobank_connections.account_id` column still has a FK to `accounts(id)`, so we create an account first. No user row needed:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::account::{Account, AccountDetails, AccountRepository, AccountType};
    use crate::infrastructure::account_repository::SqliteAccountRepository;
    use sqlx::PgPool;

    async fn make_account(pool: &PgPool) -> (Uuid, Uuid) {
        let user_id = Uuid::new_v4();
        let account = Account::new(
            user_id, "Monobank".to_string(), AccountType::Cash, "UAH".to_string(),
        );
        let account_id = account.id;
        SqliteAccountRepository::new(pool.clone())
            .create(&account, &AccountDetails::None)
            .await
            .unwrap();
        (user_id, account_id)
    }

    #[sqlx::test(migrations = "src/infrastructure/migrations")]
    async fn create_and_find_by_id(pool: PgPool) {
        let (user_id, account_id) = make_account(&pool).await;
        let repo = SqliteMonobankRepository::new(pool);
        let conn = MonobankConnection::new(
            account_id, user_id, "test-token-123".to_string(), "mono-acc-abc".to_string(),
        );
        let conn_id = conn.id;
        repo.create(&conn).await.unwrap();
        let found = repo.find_by_id(conn_id, user_id).await.unwrap().unwrap();
        assert_eq!(found.token, "test-token-123");
        assert_eq!(found.sync_status, SyncStatus::Pending);
    }

    #[sqlx::test(migrations = "src/infrastructure/migrations")]
    async fn list_incomplete_returns_pending_and_syncing(pool: PgPool) {
        let (user_id, account_id) = make_account(&pool).await;
        let repo = SqliteMonobankRepository::new(pool);

        let pending = MonobankConnection::new(
            account_id, user_id, "token-pending".to_string(), "mono-pending".to_string(),
        );
        let syncing = MonobankConnection::new(
            account_id, user_id, "token-syncing".to_string(), "mono-syncing".to_string(),
        );
        let completed = MonobankConnection::new(
            account_id, user_id, "token-completed".to_string(), "mono-completed".to_string(),
        );

        repo.create(&pending).await.unwrap();
        repo.create(&syncing).await.unwrap();
        repo.update_status(syncing.id, SyncStatus::Syncing, None).await.unwrap();
        repo.create(&completed).await.unwrap();
        repo.update_status(completed.id, SyncStatus::Completed, Some(Utc::now())).await.unwrap();

        let incomplete = repo.list_incomplete().await.unwrap();
        assert_eq!(incomplete.len(), 2);
        let statuses: Vec<&SyncStatus> = incomplete.iter().map(|c| &c.sync_status).collect();
        assert!(statuses.contains(&&SyncStatus::Pending));
        assert!(statuses.contains(&&SyncStatus::Syncing));
    }

    #[sqlx::test(migrations = "src/infrastructure/migrations")]
    async fn update_status_changes_sync_status(pool: PgPool) {
        let (user_id, account_id) = make_account(&pool).await;
        let repo = SqliteMonobankRepository::new(pool);
        let conn = MonobankConnection::new(
            account_id, user_id, "token-update".to_string(), "mono-update".to_string(),
        );
        let conn_id = conn.id;
        repo.create(&conn).await.unwrap();
        repo.update_status(conn_id, SyncStatus::Completed, Some(Utc::now())).await.unwrap();
        let found = repo.find_by_id(conn_id, user_id).await.unwrap().unwrap();
        assert_eq!(found.sync_status, SyncStatus::Completed);
        assert!(found.last_synced_at.is_some());
    }
}
```

- [ ] **Step 5: Run repository unit tests**

Requires `DATABASE_URL` pointing to a running Postgres instance:

```bash
DATABASE_URL=postgresql://postgres:password@localhost:5432/postgres cargo test --lib 2>&1 | tail -30
```

Expected: all unit tests pass.

- [ ] **Step 6: Commit**

```bash
git add src/infrastructure/account_repository.rs \
  src/infrastructure/transaction_repository.rs \
  src/infrastructure/category_repository.rs \
  src/infrastructure/monobank_repository.rs
git commit -m "test: update repository unit tests to use #[sqlx::test] with PostgreSQL"
```

---

### Task 5: Update Integration Tests

**Files:**
- Delete: `tests/api/auth.rs`
- Modify: `tests/api/helpers.rs`
- Modify: `tests/api.rs`
- Modify: `tests/api/accounts.rs`
- Modify: `tests/api/transactions.rs`
- Modify: `tests/api/categories.rs`
- Modify: `tests/api/monobank.rs`

- [ ] **Step 1: Delete `tests/api/auth.rs`**

The auth endpoints no longer exist — auth is handled entirely by Supabase.

```bash
rm tests/api/auth.rs
```

- [ ] **Step 2: Rewrite `tests/api/helpers.rs`**

Replace `register_and_login` (which called `/auth/register` and `/auth/login`) with `create_test_user()` — generates a UUID and a locally-signed JWT using the test secret. The `make_app` function now accepts a `PgPool` injected by `#[sqlx::test]`:

```rust
use std::sync::Arc;

use axum_test::TestServer;
use sqlx::PgPool;
use uuid::Uuid;

use moneykeeper::api::state::AppState;
use moneykeeper::application::accounts::AccountService;
use moneykeeper::application::categories::CategoryService;
use moneykeeper::application::monobank::MonobankService;
use moneykeeper::application::transactions::TransactionService;
use moneykeeper::domain::monobank::MonobankApiClient;
use moneykeeper::infrastructure::account_repository::SqliteAccountRepository;
use moneykeeper::infrastructure::category_repository::SqliteCategoryRepository;
use moneykeeper::infrastructure::monobank_repository::SqliteMonobankRepository;
use moneykeeper::infrastructure::transaction_repository::SqliteTransactionRepository;

/// Generate a (user_id, JWT) pair for use in test requests.
/// The JWT is signed with "test-secret" and includes the Supabase aud/role claims.
pub fn create_test_user() -> (Uuid, String) {
    let user_id = Uuid::new_v4();
    let token = test_jwt(user_id);
    (user_id, token)
}

pub fn test_jwt(user_id: Uuid) -> String {
    use jsonwebtoken::{encode, EncodingKey, Header};

    #[derive(serde::Serialize)]
    struct TestClaims {
        sub: String,
        aud: Vec<String>,
        role: String,
        exp: i64,
        iat: i64,
    }

    let now = chrono::Utc::now().timestamp();
    let claims = TestClaims {
        sub: user_id.to_string(),
        aud: vec!["authenticated".to_string()],
        role: "authenticated".to_string(),
        exp: now + 3600,
        iat: now,
    };
    encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(b"test-secret"),
    )
    .unwrap()
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
        categories: Arc::new(CategoryService::new(Arc::new(SqliteCategoryRepository::new(
            pool.clone(),
        )))),
        monobank: Arc::new(MonobankService::new(
            Arc::new(SqliteMonobankRepository::new(pool.clone())),
            tx_repo,
            monobank_client,
            "http://localhost:3000".to_string(),
        )),
        supabase_jwt_secret: "test-secret".to_string(),
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
    let res = server
        .post("/accounts")
        .add_header(
            axum::http::header::AUTHORIZATION,
            format!("Bearer {token}")
                .parse::<axum::http::HeaderValue>()
                .unwrap(),
        )
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
    let res = server
        .post("/categories")
        .add_header(
            axum::http::header::AUTHORIZATION,
            format!("Bearer {token}")
                .parse::<axum::http::HeaderValue>()
                .unwrap(),
        )
        .json(&serde_json::json!({ "name": "Test Category" }))
        .await;

    let body: serde_json::Value = res.json();
    Uuid::parse_str(body["id"].as_str().unwrap()).unwrap()
}
```

- [ ] **Step 3: Update `tests/api.rs`**

Remove the `auth` module:

```rust
#[path = "api/helpers.rs"]
mod helpers;

#[path = "api/accounts.rs"]
mod accounts;

#[path = "api/categories.rs"]
mod categories;

#[path = "api/transactions.rs"]
mod transactions;

#[path = "api/monobank.rs"]
mod monobank;
```

- [ ] **Step 4: Update `tests/api/accounts.rs`**

For every test function, apply this transformation:
- `#[tokio::test]` → `#[sqlx::test(migrations = "src/infrastructure/migrations")]`
- `async fn name()` → `async fn name(pool: sqlx::PgPool)`
- `make_app().await` → `helpers::make_app(pool).await`
- `register_and_login(&server, email, pass).await` → `helpers::create_test_user()`
  - old: `let (_uid, token, _) = register_and_login(...)` → new: `let (_uid, token) = helpers::create_test_user();`
- Remove the local `fn auth(...)` helper — use `helpers::auth(...)` from the helpers module instead (it's already re-exported there)

Example — first test becomes:

```rust
#[sqlx::test(migrations = "src/infrastructure/migrations")]
async fn create_account_cash_returns_201(pool: sqlx::PgPool) {
    let server = helpers::make_app(pool).await;
    let (_uid, token) = helpers::create_test_user();

    let res = server
        .post("/accounts")
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .json(&serde_json::json!({
            "name": "Wallet",
            "account_type": "Cash",
            "currency": "USD"
        }))
        .await;

    assert_eq!(res.status_code(), StatusCode::CREATED);
    // ...
}
```

Apply the same pattern to all 13 tests in the file. For the multi-user test `list_accounts_returns_only_own`, both calls to `register_and_login` become independent `create_test_user()` calls — they produce different UUIDs so isolation is maintained.

- [ ] **Step 5: Update `tests/api/categories.rs` and `tests/api/transactions.rs`**

Apply the identical transformation as Step 4 to every test in both files.

- [ ] **Step 6: Update `tests/api/monobank.rs`**

Apply the same transformation. The one test using `make_app_with_client` becomes:

```rust
#[sqlx::test(migrations = "src/infrastructure/migrations")]
async fn get_client_info_proxies_to_monobank(pool: sqlx::PgPool) {
    let client = Arc::new(helpers::MockMonobankClient {
        accounts: vec![moneykeeper::domain::monobank::MonoAccount {
            id: "acc-1".into(),
            currency_code: 980,
            balance: 50000,
            credit_limit: 0,
            account_type: "black".into(),
            iban: Some("UA123456789".into()),
        }],
        statement_items: vec![],
    });
    let server = helpers::make_app_with_client(pool, client).await;
    let (_uid, token) = helpers::create_test_user();
    // ... rest of test unchanged
}
```

- [ ] **Step 7: Run all integration tests**

```bash
DATABASE_URL=postgresql://postgres:password@localhost:5432/postgres cargo test --test api 2>&1 | tail -30
```

Expected: all tests pass.

- [ ] **Step 8: Run clippy and format check**

```bash
cargo clippy -- -D warnings 2>&1 | head -40
cargo fmt --check
```

Fix any issues before committing.

- [ ] **Step 9: Commit**

```bash
git add tests/api.rs tests/api/helpers.rs tests/api/accounts.rs \
  tests/api/transactions.rs tests/api/categories.rs tests/api/monobank.rs
git commit -m "test: update integration tests for Supabase auth (no register/login, #[sqlx::test])"
```

---

## Chunk 4: Deployment

### Task 6: Dockerfile, .env, and sqlx Offline Mode

**Files:**
- Modify: `Dockerfile`
- Modify: `.env`
- Create (generated): `.sqlx/` directory (via `cargo sqlx prepare`)

- [ ] **Step 1: Rewrite `Dockerfile`**

```dockerfile
# ─── Build Stage ─────────────────────────────────────────────────────────────
FROM rust:1-alpine AS builder

RUN apk add --no-cache musl-dev pkgconf
# sqlite-dev and sqlite-static removed — no longer needed

WORKDIR /app

ENV SQLX_OFFLINE=true

# Cache dependencies layer
COPY Cargo.toml Cargo.lock ./
RUN mkdir -p src && \
    echo "fn main() {}" > src/main.rs && \
    touch src/lib.rs && \
    cargo build --release 2>/dev/null || true && \
    rm -rf src

# Build the real binary — .sqlx/ cache must exist for SQLX_OFFLINE=true
COPY .sqlx ./.sqlx
COPY src ./src
RUN touch src/main.rs src/lib.rs && cargo build --release

# ─── Runtime Stage ───────────────────────────────────────────────────────────
FROM alpine:3.21
RUN apk add --no-cache ca-certificates tzdata
WORKDIR /app
COPY --from=builder /app/target/release/moneykeeper .

ENV RUST_LOG=info
ENV BIND_ADDR=0.0.0.0:8080
# DATABASE_URL and SUPABASE_JWT_SECRET are injected via fly secrets — not set here
# Removed: DATABASE_URL default, JWT_SECRET, VOLUME /data

EXPOSE 8080
ENTRYPOINT ["./moneykeeper"]
```

- [ ] **Step 2: Update `.env`**

```
DATABASE_URL=postgresql://postgres:[password]@db.[project-ref].supabase.co:5432/postgres
SUPABASE_JWT_SECRET=<from Supabase Dashboard → Settings → API → JWT Secret>
PUBLIC_URL=https://[your-app].fly.dev
BIND_ADDR=0.0.0.0:8080
RUST_LOG=info
```

Note: port 5432 = direct Postgres (session mode). Do **not** use 6543 (PgBouncer) — that's for serverless/short-lived connections.

- [ ] **Step 3: Generate sqlx offline cache**

This step requires a live Supabase database. Export `DATABASE_URL` explicitly (sqlx reads from environment, not `.env`):

```bash
export DATABASE_URL="postgresql://postgres:[password]@db.[project-ref].supabase.co:5432/postgres"
cargo sqlx prepare
```

This creates a `.sqlx/` directory with compile-time query metadata. Commit it:

```bash
git add .sqlx/
git commit -m "chore: add sqlx offline cache for Fly.io builds"
```

- [ ] **Step 4: Configure Fly.io secrets**

```bash
fly secrets set \
  DATABASE_URL="postgresql://..." \
  SUPABASE_JWT_SECRET="..." \
  PUBLIC_URL="https://[your-app].fly.dev" \
  BIND_ADDR="0.0.0.0:8080"
```

- [ ] **Step 5: Verify `fly.toml` has correct settings**

Ensure these sections exist in `fly.toml`:

```toml
[http_service]
  internal_port = 8080
  force_https = true

[[vm]]
  memory = "256mb"
  cpu_kind = "shared"
  cpus = 1
```

- [ ] **Step 6: Final verification**

```bash
cargo test 2>&1 | tail -20
cargo clippy -- -D warnings
cargo fmt --check
```

Expected: all pass, no warnings, no format diffs.

- [ ] **Step 7: Commit remaining files**

```bash
git add Dockerfile .env
git commit -m "chore: update Dockerfile for PostgreSQL and configure Fly.io deployment"
```