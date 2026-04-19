# Monobank Sync Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Sync transactions from Monobank into moneykeeper via initial history pull + webhook.

**Architecture:** DDD layers — domain trait, application service with background Tokio task, SQLite repository, Axum handlers. `MonobankApiClient` is a trait so tests can use a mock. On startup, incomplete syncs are restarted.

**Tech Stack:** `reqwest` (Monobank HTTP), `tokio::spawn` (background sync), `axum-test` + mock client (tests), existing SQLx/SQLite patterns.

---

### Task 1: DB Migrations

**Files:**
- Create: `src/infrastructure/migrations/004_external_id.sql`
- Create: `src/infrastructure/migrations/005_monobank_connections.sql`

**Step 1: Write 004_external_id.sql**

```sql
-- SQLite cannot add UNIQUE via ALTER TABLE; use a partial unique index instead
ALTER TABLE transactions ADD COLUMN external_id TEXT;
CREATE UNIQUE INDEX idx_transactions_external_id
    ON transactions(external_id)
    WHERE external_id IS NOT NULL;
```

**Step 2: Write 005_monobank_connections.sql**

```sql
CREATE TABLE monobank_connections (
    id TEXT PRIMARY KEY NOT NULL,
    account_id TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    token TEXT NOT NULL,
    monobank_account_id TEXT NOT NULL,
    sync_status TEXT NOT NULL DEFAULT 'pending',
    last_synced_at INTEGER,
    created_at INTEGER NOT NULL
);

CREATE INDEX idx_monobank_connections_user_id ON monobank_connections(user_id);
CREATE INDEX idx_monobank_connections_account_id ON monobank_connections(account_id);
CREATE UNIQUE INDEX idx_monobank_connections_monobank_account_id
    ON monobank_connections(monobank_account_id);
```

**Step 3: Verify migrations apply**

```bash
cargo build
```
Expected: compiles without errors (sqlx::migrate! picks up new files automatically).

**Step 4: Commit**

```bash
git add src/infrastructure/migrations/004_external_id.sql src/infrastructure/migrations/005_monobank_connections.sql
git commit -m "feat: add external_id to transactions and monobank_connections table"
```

---

### Task 2: Update Transaction Domain Entity

**Files:**
- Modify: `src/domain/transaction.rs`

**Step 1: Add `external_id` field to `Transaction` struct**

Add field after `note`:
```rust
pub external_id: Option<String>,
```

**Step 2: Update `Transaction::new` to set `external_id: None`**

Find the `Transaction::new(...)` constructor body and add:
```rust
external_id: None,
```

**Step 3: Add `create_idempotent` to `TransactionRepository` trait**

Add this method to the `TransactionRepository` trait in the same file:
```rust
/// Insert a transaction using INSERT OR IGNORE (for external syncs).
async fn create_idempotent(&self, tx: &Transaction) -> anyhow::Result<()>;
```

**Step 4: Verify it compiles**

```bash
cargo build 2>&1 | head -40
```
Expected: errors about `create_idempotent` not implemented in `SqliteTransactionRepository` — that's fine for now.

**Step 5: Implement `create_idempotent` in SqliteTransactionRepository**

Open `src/infrastructure/transaction_repository.rs`. Add to the `impl TransactionRepository for SqliteTransactionRepository` block:
```rust
async fn create_idempotent(&self, tx: &Transaction) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT OR IGNORE INTO transactions \
         (id, account_id, user_id, amount, currency, kind, category_id, note, external_id, transacted_at, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
    )
    .bind(tx.id.to_string())
    .bind(tx.account_id.to_string())
    .bind(tx.user_id.to_string())
    .bind(tx.amount.to_string())
    .bind(&tx.currency)
    .bind(tx.kind.as_str())
    .bind(tx.category_id.map(|id| id.to_string()))
    .bind(&tx.note)
    .bind(&tx.external_id)
    .bind(tx.transacted_at.to_rfc3339())
    .bind(tx.created_at.to_rfc3339())
    .execute(&self.pool)
    .await?;
    Ok(())
}
```

Also update the existing `create` INSERT to include the `external_id` column:
```sql
INSERT INTO transactions
(id, account_id, user_id, amount, currency, kind, category_id, note, external_id, transacted_at, created_at)
VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
```
Add `.bind(&tx.external_id)` after the `.bind(&tx.note)` line.

Also update `TxRow` struct to include:
```rust
external_id: Option<String>,
```
And update `row_to_tx` to set:
```rust
external_id: r.external_id,
```

**Step 6: Verify compiles and tests pass**

```bash
cargo test 2>&1 | tail -20
```
Expected: all existing tests pass.

**Step 7: Commit**

```bash
git add src/domain/transaction.rs src/infrastructure/transaction_repository.rs
git commit -m "feat: add external_id to Transaction and create_idempotent repository method"
```

---

### Task 3: Domain — MonobankConnection Entity + Repository Trait

**Files:**
- Create: `src/domain/monobank.rs`
- Modify: `src/domain/mod.rs`

**Step 1: Create `src/domain/monobank.rs`**

```rust
use chrono::{DateTime, Utc};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq)]
pub enum SyncStatus {
    Pending,
    Syncing,
    Completed,
    Failed,
}

impl SyncStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            SyncStatus::Pending => "pending",
            SyncStatus::Syncing => "syncing",
            SyncStatus::Completed => "completed",
            SyncStatus::Failed => "failed",
        }
    }

    pub fn from_str(s: &str) -> anyhow::Result<Self> {
        match s {
            "pending" => Ok(SyncStatus::Pending),
            "syncing" => Ok(SyncStatus::Syncing),
            "completed" => Ok(SyncStatus::Completed),
            "failed" => Ok(SyncStatus::Failed),
            other => Err(anyhow::anyhow!("unknown sync_status: {other}")),
        }
    }
}

#[derive(Debug, Clone)]
pub struct MonobankConnection {
    pub id: Uuid,
    pub account_id: Uuid,
    pub user_id: Uuid,
    pub token: String,
    pub monobank_account_id: String,
    pub sync_status: SyncStatus,
    pub last_synced_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

impl MonobankConnection {
    pub fn new(
        account_id: Uuid,
        user_id: Uuid,
        token: String,
        monobank_account_id: String,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            account_id,
            user_id,
            token,
            monobank_account_id,
            sync_status: SyncStatus::Pending,
            last_synced_at: None,
            created_at: Utc::now(),
        }
    }
}

#[async_trait::async_trait]
pub trait MonobankConnectionRepository: Send + Sync {
    async fn create(&self, conn: &MonobankConnection) -> anyhow::Result<()>;
    async fn find_by_id(&self, id: Uuid, user_id: Uuid) -> anyhow::Result<Option<MonobankConnection>>;
    async fn find_by_monobank_account_id(&self, monobank_account_id: &str) -> anyhow::Result<Option<MonobankConnection>>;
    async fn list_by_user(&self, user_id: Uuid) -> anyhow::Result<Vec<MonobankConnection>>;
    async fn list_incomplete(&self) -> anyhow::Result<Vec<MonobankConnection>>;
    async fn update_status(
        &self,
        id: Uuid,
        status: SyncStatus,
        last_synced_at: Option<DateTime<Utc>>,
    ) -> anyhow::Result<()>;
    async fn delete(&self, id: Uuid, user_id: Uuid) -> anyhow::Result<()>;
}
```

**Step 2: Register module in `src/domain/mod.rs`**

Add:
```rust
pub mod monobank;
```

**Step 3: Verify**

```bash
cargo build 2>&1 | head -20
```
Expected: compiles.

**Step 4: Commit**

```bash
git add src/domain/monobank.rs src/domain/mod.rs
git commit -m "feat(domain): add MonobankConnection entity and repository trait"
```

---

### Task 4: Monobank API Client Trait + Reqwest Implementation

**Files:**
- Modify: `Cargo.toml`
- Create: `src/infrastructure/monobank_client.rs`
- Modify: `src/infrastructure/mod.rs`

**Step 1: Add `reqwest` to Cargo.toml**

Add under `[dependencies]`:
```toml
reqwest = { version = "0.12", features = ["json", "rustls-tls"], default-features = false }
```

**Step 2: Create `src/infrastructure/monobank_client.rs`**

```rust
use anyhow::Context;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::Deserialize;

/// A single Monobank account (card) from client-info response.
#[derive(Debug, Clone, Deserialize)]
pub struct MonoAccount {
    pub id: String,
    #[serde(rename = "currencyCode")]
    pub currency_code: u16,
    pub balance: i64,
    #[serde(rename = "creditLimit")]
    pub credit_limit: i64,
    #[serde(rename = "type")]
    pub account_type: String,
    pub iban: Option<String>,
}

/// A single transaction from Monobank statement.
#[derive(Debug, Clone, Deserialize)]
pub struct MonoStatementItem {
    pub id: String,
    pub time: i64,
    pub description: Option<String>,
    pub mcc: i32,
    pub amount: i64,
    #[serde(rename = "operationAmount")]
    pub operation_amount: i64,
    #[serde(rename = "currencyCode")]
    pub currency_code: u16,
    pub balance: i64,
    pub hold: bool,
}

impl MonoStatementItem {
    /// Convert amount in kopecks (1/100 UAH) to Decimal.
    pub fn amount_decimal(&self) -> Decimal {
        Decimal::new(self.amount.abs(), 2)
    }

    /// true = income, false = expense
    pub fn is_income(&self) -> bool {
        self.amount > 0
    }

    pub fn transacted_at(&self) -> DateTime<Utc> {
        DateTime::from_timestamp(self.time, 0).unwrap_or_else(Utc::now)
    }
}

#[async_trait::async_trait]
pub trait MonobankApiClient: Send + Sync {
    /// Fetch client info and available accounts.
    async fn get_accounts(&self, token: &str) -> anyhow::Result<Vec<MonoAccount>>;

    /// Fetch statement for a date range (max 31 days per call, rate-limited to 1/min).
    async fn get_statement(
        &self,
        token: &str,
        account_id: &str,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> anyhow::Result<Vec<MonoStatementItem>>;

    /// Register a webhook URL for the given token.
    async fn set_webhook(&self, token: &str, webhook_url: &str) -> anyhow::Result<()>;
}

pub struct ReqwestMonobankClient {
    client: reqwest::Client,
    base_url: String,
}

impl ReqwestMonobankClient {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: "https://api.monobank.ua".to_string(),
        }
    }

    /// For tests or overriding the base URL.
    pub fn with_base_url(base_url: String) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url,
        }
    }
}

#[derive(Deserialize)]
struct ClientInfo {
    accounts: Vec<MonoAccount>,
}

#[async_trait::async_trait]
impl MonobankApiClient for ReqwestMonobankClient {
    async fn get_accounts(&self, token: &str) -> anyhow::Result<Vec<MonoAccount>> {
        let info: ClientInfo = self
            .client
            .get(format!("{}/personal/client-info", self.base_url))
            .header("X-Token", token)
            .send()
            .await
            .context("monobank client-info request failed")?
            .error_for_status()
            .context("monobank client-info non-2xx")?
            .json()
            .await
            .context("monobank client-info parse failed")?;
        Ok(info.accounts)
    }

    async fn get_statement(
        &self,
        token: &str,
        account_id: &str,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> anyhow::Result<Vec<MonoStatementItem>> {
        let from_ts = from.timestamp();
        let to_ts = to.timestamp();
        let items: Vec<MonoStatementItem> = self
            .client
            .get(format!(
                "{}/personal/statement/{account_id}/{from_ts}/{to_ts}",
                self.base_url
            ))
            .header("X-Token", token)
            .send()
            .await
            .context("monobank statement request failed")?
            .error_for_status()
            .context("monobank statement non-2xx")?
            .json()
            .await
            .context("monobank statement parse failed")?;
        Ok(items)
    }

    async fn set_webhook(&self, token: &str, webhook_url: &str) -> anyhow::Result<()> {
        self.client
            .post(format!("{}/personal/webhook", self.base_url))
            .header("X-Token", token)
            .json(&serde_json::json!({ "webHookUrl": webhook_url }))
            .send()
            .await
            .context("monobank set-webhook request failed")?
            .error_for_status()
            .context("monobank set-webhook non-2xx")?;
        Ok(())
    }
}
```

**Step 3: Register in `src/infrastructure/mod.rs`**

Add:
```rust
pub mod monobank_client;
```

**Step 4: Verify**

```bash
cargo build 2>&1 | head -20
```
Expected: compiles (reqwest downloaded).

**Step 5: Commit**

```bash
git add Cargo.toml src/infrastructure/monobank_client.rs src/infrastructure/mod.rs
git commit -m "feat(infra): add MonobankApiClient trait and reqwest implementation"
```

---

### Task 5: MonobankConnectionRepository SQLite Implementation

**Files:**
- Create: `src/infrastructure/monobank_repository.rs`
- Modify: `src/infrastructure/mod.rs`

**Step 1: Write the failing test first** (at the bottom of `src/infrastructure/monobank_repository.rs` before implementing)

Create the file with an empty impl stub + test:

```rust
use anyhow::Context;
use chrono::{DateTime, Utc};
use sqlx::SqlitePool;
use uuid::Uuid;

use crate::domain::monobank::{MonobankConnection, MonobankConnectionRepository, SyncStatus};

pub struct SqliteMonobankRepository {
    pool: SqlitePool,
}

impl SqliteMonobankRepository {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

#[derive(sqlx::FromRow)]
struct ConnectionRow {
    id: String,
    account_id: String,
    user_id: String,
    token: String,
    monobank_account_id: String,
    sync_status: String,
    last_synced_at: Option<i64>,
    created_at: i64,
}

fn row_to_conn(r: ConnectionRow) -> anyhow::Result<MonobankConnection> {
    Ok(MonobankConnection {
        id: Uuid::parse_str(&r.id)?,
        account_id: Uuid::parse_str(&r.account_id)?,
        user_id: Uuid::parse_str(&r.user_id)?,
        token: r.token,
        monobank_account_id: r.monobank_account_id,
        sync_status: SyncStatus::from_str(&r.sync_status)?,
        last_synced_at: r.last_synced_at.map(|ts| {
            DateTime::from_timestamp(ts, 0).unwrap_or_else(Utc::now)
        }),
        created_at: DateTime::from_timestamp(r.created_at, 0).unwrap_or_else(Utc::now),
    })
}

#[async_trait::async_trait]
impl MonobankConnectionRepository for SqliteMonobankRepository {
    async fn create(&self, conn: &MonobankConnection) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO monobank_connections \
             (id, account_id, user_id, token, monobank_account_id, sync_status, last_synced_at, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)"
        )
        .bind(conn.id.to_string())
        .bind(conn.account_id.to_string())
        .bind(conn.user_id.to_string())
        .bind(&conn.token)
        .bind(&conn.monobank_account_id)
        .bind(conn.sync_status.as_str())
        .bind(conn.last_synced_at.map(|dt| dt.timestamp()))
        .bind(conn.created_at.timestamp())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn find_by_id(&self, id: Uuid, user_id: Uuid) -> anyhow::Result<Option<MonobankConnection>> {
        let row = sqlx::query_as::<_, ConnectionRow>(
            "SELECT * FROM monobank_connections WHERE id = ? AND user_id = ?"
        )
        .bind(id.to_string())
        .bind(user_id.to_string())
        .fetch_optional(&self.pool)
        .await?;
        row.map(row_to_conn).transpose()
    }

    async fn find_by_monobank_account_id(&self, monobank_account_id: &str) -> anyhow::Result<Option<MonobankConnection>> {
        let row = sqlx::query_as::<_, ConnectionRow>(
            "SELECT * FROM monobank_connections WHERE monobank_account_id = ?"
        )
        .bind(monobank_account_id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(row_to_conn).transpose()
    }

    async fn list_by_user(&self, user_id: Uuid) -> anyhow::Result<Vec<MonobankConnection>> {
        let rows = sqlx::query_as::<_, ConnectionRow>(
            "SELECT * FROM monobank_connections WHERE user_id = ? ORDER BY created_at DESC"
        )
        .bind(user_id.to_string())
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(row_to_conn).collect()
    }

    async fn list_incomplete(&self) -> anyhow::Result<Vec<MonobankConnection>> {
        let rows = sqlx::query_as::<_, ConnectionRow>(
            "SELECT * FROM monobank_connections WHERE sync_status IN ('pending', 'syncing')"
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
            "UPDATE monobank_connections SET sync_status = ?, last_synced_at = ? WHERE id = ?"
        )
        .bind(status.as_str())
        .bind(last_synced_at.map(|dt| dt.timestamp()))
        .bind(id.to_string())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn delete(&self, id: Uuid, user_id: Uuid) -> anyhow::Result<()> {
        sqlx::query(
            "DELETE FROM monobank_connections WHERE id = ? AND user_id = ?"
        )
        .bind(id.to_string())
        .bind(user_id.to_string())
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::SqlitePool;

    async fn make_pool() -> SqlitePool {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::migrate!("src/infrastructure/migrations").run(&pool).await.unwrap();
        pool
    }

    #[tokio::test]
    async fn create_and_find_by_id() {
        let pool = make_pool().await;
        let repo = SqliteMonobankRepository::new(pool);

        let user_id = Uuid::new_v4();
        let account_id = Uuid::new_v4();
        let conn = MonobankConnection::new(
            account_id,
            user_id,
            "test-token".to_string(),
            "mono-acc-1".to_string(),
        );
        let id = conn.id;

        repo.create(&conn).await.unwrap();

        let found = repo.find_by_id(id, user_id).await.unwrap();
        assert!(found.is_some());
        let found = found.unwrap();
        assert_eq!(found.token, "test-token");
        assert_eq!(found.sync_status, SyncStatus::Pending);
    }

    #[tokio::test]
    async fn list_incomplete_returns_pending_and_syncing() {
        let pool = make_pool().await;
        let repo = SqliteMonobankRepository::new(pool);

        let user_id = Uuid::new_v4();
        let account_id = Uuid::new_v4();
        let mut conn1 = MonobankConnection::new(account_id, user_id, "t1".to_string(), "acc1".to_string());
        let mut conn2 = MonobankConnection::new(account_id, user_id, "t2".to_string(), "acc2".to_string());
        let mut conn3 = MonobankConnection::new(account_id, user_id, "t3".to_string(), "acc3".to_string());
        conn2.sync_status = SyncStatus::Syncing;
        conn3.sync_status = SyncStatus::Completed;

        repo.create(&conn1).await.unwrap();
        repo.create(&conn2).await.unwrap();
        repo.create(&conn3).await.unwrap();

        let incomplete = repo.list_incomplete().await.unwrap();
        assert_eq!(incomplete.len(), 2);
    }

    #[tokio::test]
    async fn update_status_changes_sync_status() {
        let pool = make_pool().await;
        let repo = SqliteMonobankRepository::new(pool);

        let user_id = Uuid::new_v4();
        let conn = MonobankConnection::new(Uuid::new_v4(), user_id, "t".to_string(), "acc".to_string());
        let id = conn.id;
        repo.create(&conn).await.unwrap();

        repo.update_status(id, SyncStatus::Completed, Some(Utc::now())).await.unwrap();

        let found = repo.find_by_id(id, user_id).await.unwrap().unwrap();
        assert_eq!(found.sync_status, SyncStatus::Completed);
        assert!(found.last_synced_at.is_some());
    }
}
```

**Step 2: Run tests**

```bash
cargo test monobank_repository 2>&1 | tail -20
```
Expected: all 3 unit tests pass.

**Step 3: Register module in `src/infrastructure/mod.rs`**

Add:
```rust
pub mod monobank_repository;
```

**Step 4: Verify full build + tests**

```bash
cargo test 2>&1 | tail -20
```
Expected: all tests pass.

**Step 5: Commit**

```bash
git add src/infrastructure/monobank_repository.rs src/infrastructure/mod.rs
git commit -m "feat(infra): add SqliteMonobankRepository with unit tests"
```

---

### Task 6: Application Service — MonobankService

**Files:**
- Create: `src/application/monobank.rs`
- Modify: `src/application/mod.rs`

**Step 1: Create `src/application/monobank.rs`**

```rust
use std::sync::Arc;

use chrono::{Duration, Utc};
use uuid::Uuid;

use crate::domain::monobank::{MonobankConnection, MonobankConnectionRepository, SyncStatus};
use crate::domain::transaction::{Transaction, TransactionKind, TransactionRepository};
use crate::infrastructure::monobank_client::{MonobankApiClient, MonoStatementItem};

pub struct MonobankService {
    connection_repo: Arc<dyn MonobankConnectionRepository>,
    transaction_repo: Arc<dyn TransactionRepository>,
    monobank_client: Arc<dyn MonobankApiClient>,
    public_url: String,
}

impl MonobankService {
    pub fn new(
        connection_repo: Arc<dyn MonobankConnectionRepository>,
        transaction_repo: Arc<dyn TransactionRepository>,
        monobank_client: Arc<dyn MonobankApiClient>,
        public_url: String,
    ) -> Self {
        Self {
            connection_repo,
            transaction_repo,
            monobank_client,
            public_url,
        }
    }

    pub async fn get_monobank_accounts(
        &self,
        token: &str,
    ) -> anyhow::Result<Vec<crate::infrastructure::monobank_client::MonoAccount>> {
        self.monobank_client.get_accounts(token).await
    }

    /// Create a connection, save it as pending, and spawn a background sync.
    pub async fn connect(
        &self,
        account_id: Uuid,
        user_id: Uuid,
        token: String,
        monobank_account_id: String,
        account_created_at: chrono::DateTime<Utc>,
    ) -> anyhow::Result<MonobankConnection> {
        let conn = MonobankConnection::new(account_id, user_id, token, monobank_account_id);
        self.connection_repo.create(&conn).await?;
        self.spawn_sync(conn.clone(), account_created_at);
        Ok(conn)
    }

    pub async fn list_connections(&self, user_id: Uuid) -> anyhow::Result<Vec<MonobankConnection>> {
        self.connection_repo.list_by_user(user_id).await
    }

    pub async fn delete_connection(&self, id: Uuid, user_id: Uuid) -> anyhow::Result<()> {
        let conn = self
            .connection_repo
            .find_by_id(id, user_id)
            .await?
            .ok_or_else(|| crate::domain::error::DomainError::NotFound(format!("connection {id}")))?;
        if conn.user_id != user_id {
            return Err(crate::domain::error::DomainError::Unauthorized.into());
        }
        self.connection_repo.delete(id, user_id).await
    }

    /// Handle a webhook payload from Monobank. Returns the number of transactions inserted.
    pub async fn handle_webhook(
        &self,
        monobank_account_id: &str,
        item: &MonoStatementItem,
    ) -> anyhow::Result<usize> {
        let conn = match self
            .connection_repo
            .find_by_monobank_account_id(monobank_account_id)
            .await?
        {
            Some(c) => c,
            None => {
                tracing::warn!("received webhook for unknown monobank account: {monobank_account_id}");
                return Ok(0);
            }
        };

        let inserted = self.insert_statement_item(&conn, item).await?;
        Ok(if inserted { 1 } else { 0 })
    }

    /// Restart any pending/syncing connections on startup.
    pub async fn restart_incomplete_syncs(&self, pool: sqlx::SqlitePool) {
        match self.connection_repo.list_incomplete().await {
            Ok(connections) => {
                for conn in connections {
                    // Reset to pending before restarting
                    if let Err(e) = self
                        .connection_repo
                        .update_status(conn.id, SyncStatus::Pending, None)
                        .await
                    {
                        tracing::error!("failed to reset connection {}: {e}", conn.id);
                        continue;
                    }
                    // Use account created_at as the earliest sync date
                    let from = conn.created_at;
                    self.spawn_sync(conn, from);
                }
            }
            Err(e) => tracing::error!("failed to list incomplete syncs: {e}"),
        }
    }

    /// Spawn background sync task for a connection.
    pub fn spawn_sync(&self, conn: MonobankConnection, from: chrono::DateTime<Utc>) {
        let connection_repo = Arc::clone(&self.connection_repo);
        let transaction_repo = Arc::clone(&self.transaction_repo);
        let monobank_client = Arc::clone(&self.monobank_client);

        tokio::spawn(async move {
            if let Err(e) = run_sync(connection_repo, transaction_repo, monobank_client, conn, from).await {
                tracing::error!("monobank sync error: {e}");
            }
        });
    }

    async fn insert_statement_item(
        &self,
        conn: &MonobankConnection,
        item: &MonoStatementItem,
    ) -> anyhow::Result<bool> {
        let kind = if item.is_income() {
            TransactionKind::Income
        } else {
            TransactionKind::Expense
        };
        let mut tx = Transaction::new(
            conn.account_id,
            conn.user_id,
            item.amount_decimal(),
            "UAH".to_string(),
            kind,
            None,
            item.description.clone(),
            item.transacted_at(),
        );
        tx.external_id = Some(item.id.clone());
        self.transaction_repo.create_idempotent(&tx).await?;
        Ok(true)
    }
}

async fn run_sync(
    connection_repo: Arc<dyn MonobankConnectionRepository>,
    transaction_repo: Arc<dyn TransactionRepository>,
    monobank_client: Arc<dyn MonobankApiClient>,
    conn: MonobankConnection,
    history_from: chrono::DateTime<Utc>,
) -> anyhow::Result<()> {
    // Mark as syncing
    connection_repo
        .update_status(conn.id, SyncStatus::Syncing, None)
        .await?;

    // Register webhook (best effort — don't fail sync if this fails)
    let webhook_url = format!("{}/monobank/webhook", ""); // placeholder — see Task 8
    if let Err(e) = monobank_client.set_webhook(&conn.token, &webhook_url).await {
        tracing::warn!("failed to set webhook: {e}");
    }

    // Fetch history in 31-day chunks
    let chunk = Duration::days(31);
    let mut cursor = history_from;
    let now = Utc::now();

    while cursor < now {
        let to = (cursor + chunk).min(now);
        match monobank_client
            .get_statement(&conn.token, &conn.monobank_account_id, cursor, to)
            .await
        {
            Ok(items) => {
                for item in &items {
                    let kind = if item.is_income() {
                        TransactionKind::Income
                    } else {
                        TransactionKind::Expense
                    };
                    let mut tx = Transaction::new(
                        conn.account_id,
                        conn.user_id,
                        item.amount_decimal(),
                        "UAH".to_string(),
                        kind,
                        None,
                        item.description.clone(),
                        item.transacted_at(),
                    );
                    tx.external_id = Some(item.id.clone());
                    if let Err(e) = transaction_repo.create_idempotent(&tx).await {
                        tracing::error!("failed to insert monobank tx {}: {e}", item.id);
                    }
                }
            }
            Err(e) => {
                tracing::error!("failed to fetch statement chunk: {e}");
                connection_repo
                    .update_status(conn.id, SyncStatus::Failed, None)
                    .await?;
                return Err(e);
            }
        }

        cursor = to;
        if cursor < now {
            // Rate limit: 1 request per minute
            tokio::time::sleep(tokio::time::Duration::from_secs(61)).await;
        }
    }

    connection_repo
        .update_status(conn.id, SyncStatus::Completed, Some(Utc::now()))
        .await?;
    tracing::info!("monobank sync completed for connection {}", conn.id);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::monobank::MonobankConnectionRepository;
    use crate::domain::transaction::TransactionDetails;
    use crate::infrastructure::monobank_client::{MonoAccount, MonoStatementItem};
    use std::sync::Mutex;

    // --- Mock implementations ---

    struct MockConnectionRepo {
        connections: Mutex<Vec<MonobankConnection>>,
    }

    impl MockConnectionRepo {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                connections: Mutex::new(vec![]),
            })
        }
    }

    #[async_trait::async_trait]
    impl MonobankConnectionRepository for MockConnectionRepo {
        async fn create(&self, conn: &MonobankConnection) -> anyhow::Result<()> {
            self.connections.lock().unwrap().push(conn.clone());
            Ok(())
        }
        async fn find_by_id(&self, id: Uuid, _user_id: Uuid) -> anyhow::Result<Option<MonobankConnection>> {
            Ok(self.connections.lock().unwrap().iter().find(|c| c.id == id).cloned())
        }
        async fn find_by_monobank_account_id(&self, mono_id: &str) -> anyhow::Result<Option<MonobankConnection>> {
            Ok(self.connections.lock().unwrap().iter().find(|c| c.monobank_account_id == mono_id).cloned())
        }
        async fn list_by_user(&self, user_id: Uuid) -> anyhow::Result<Vec<MonobankConnection>> {
            Ok(self.connections.lock().unwrap().iter().filter(|c| c.user_id == user_id).cloned().collect())
        }
        async fn list_incomplete(&self) -> anyhow::Result<Vec<MonobankConnection>> {
            Ok(self.connections.lock().unwrap().iter()
                .filter(|c| matches!(c.sync_status, SyncStatus::Pending | SyncStatus::Syncing))
                .cloned().collect())
        }
        async fn update_status(&self, id: Uuid, status: SyncStatus, last_synced_at: Option<chrono::DateTime<Utc>>) -> anyhow::Result<()> {
            let mut conns = self.connections.lock().unwrap();
            if let Some(c) = conns.iter_mut().find(|c| c.id == id) {
                c.sync_status = status;
                c.last_synced_at = last_synced_at;
            }
            Ok(())
        }
        async fn delete(&self, id: Uuid, _user_id: Uuid) -> anyhow::Result<()> {
            self.connections.lock().unwrap().retain(|c| c.id != id);
            Ok(())
        }
    }

    struct MockTransactionRepo {
        txs: Mutex<Vec<Transaction>>,
    }

    impl MockTransactionRepo {
        fn new() -> Arc<Self> {
            Arc::new(Self { txs: Mutex::new(vec![]) })
        }
    }

    #[async_trait::async_trait]
    impl TransactionRepository for MockTransactionRepo {
        async fn create(&self, tx: &Transaction, _details: &TransactionDetails) -> anyhow::Result<()> {
            self.txs.lock().unwrap().push(tx.clone());
            Ok(())
        }
        async fn create_idempotent(&self, tx: &Transaction) -> anyhow::Result<()> {
            let mut txs = self.txs.lock().unwrap();
            let already = txs.iter().any(|t| t.external_id == tx.external_id && tx.external_id.is_some());
            if !already {
                txs.push(tx.clone());
            }
            Ok(())
        }
        async fn find_by_id(&self, id: Uuid, _user_id: Uuid) -> anyhow::Result<Option<(Transaction, TransactionDetails)>> {
            Ok(self.txs.lock().unwrap().iter().find(|t| t.id == id).map(|t| (t.clone(), TransactionDetails::None)))
        }
        async fn list(&self, _params: &crate::domain::transaction::TransactionListParams) -> anyhow::Result<Vec<(Transaction, TransactionDetails)>> {
            Ok(self.txs.lock().unwrap().iter().map(|t| (t.clone(), TransactionDetails::None)).collect())
        }
        async fn update(&self, _tx: &Transaction, _details: &TransactionDetails) -> anyhow::Result<()> { Ok(()) }
        async fn delete(&self, _id: Uuid, _user_id: Uuid) -> anyhow::Result<()> { Ok(()) }
    }

    struct MockMonobankClient {
        accounts: Vec<MonoAccount>,
        statement_items: Vec<MonoStatementItem>,
    }

    impl MockMonobankClient {
        fn empty() -> Arc<Self> {
            Arc::new(Self { accounts: vec![], statement_items: vec![] })
        }
        fn with_items(items: Vec<MonoStatementItem>) -> Arc<Self> {
            Arc::new(Self { accounts: vec![], statement_items: items })
        }
    }

    #[async_trait::async_trait]
    impl MonobankApiClient for MockMonobankClient {
        async fn get_accounts(&self, _token: &str) -> anyhow::Result<Vec<MonoAccount>> {
            Ok(self.accounts.clone())
        }
        async fn get_statement(&self, _token: &str, _account_id: &str, _from: chrono::DateTime<Utc>, _to: chrono::DateTime<Utc>) -> anyhow::Result<Vec<MonoStatementItem>> {
            Ok(self.statement_items.clone())
        }
        async fn set_webhook(&self, _token: &str, _url: &str) -> anyhow::Result<()> { Ok(()) }
    }

    fn make_service(
        conn_repo: Arc<dyn MonobankConnectionRepository>,
        tx_repo: Arc<dyn TransactionRepository>,
        client: Arc<dyn MonobankApiClient>,
    ) -> MonobankService {
        MonobankService::new(conn_repo, tx_repo, client, "http://localhost".to_string())
    }

    #[tokio::test]
    async fn connect_saves_connection_as_pending() {
        let conn_repo = MockConnectionRepo::new();
        let tx_repo = MockTransactionRepo::new();
        let client = MockMonobankClient::empty();
        let svc = make_service(conn_repo.clone(), tx_repo, client);

        let user_id = Uuid::new_v4();
        let account_id = Uuid::new_v4();
        let result = svc
            .connect(account_id, user_id, "tok".into(), "mono-1".into(), Utc::now())
            .await
            .unwrap();

        assert_eq!(result.sync_status, SyncStatus::Pending);
        assert_eq!(conn_repo.connections.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn handle_webhook_inserts_income_transaction() {
        let conn_repo = MockConnectionRepo::new();
        let tx_repo = MockTransactionRepo::new();
        let client = MockMonobankClient::empty();

        let user_id = Uuid::new_v4();
        let account_id = Uuid::new_v4();
        let conn = MonobankConnection::new(account_id, user_id, "tok".into(), "mono-acc".into());
        conn_repo.create(&conn).await.unwrap();

        let svc = make_service(conn_repo, tx_repo.clone(), client);

        let item = MonoStatementItem {
            id: "ext-123".into(),
            time: Utc::now().timestamp(),
            description: Some("Coffee".into()),
            mcc: 5411,
            amount: 5000,
            operation_amount: 5000,
            currency_code: 980,
            balance: 100000,
            hold: false,
        };

        let count = svc.handle_webhook("mono-acc", &item).await.unwrap();
        assert_eq!(count, 1);

        let txs = tx_repo.txs.lock().unwrap();
        assert_eq!(txs.len(), 1);
        assert_eq!(txs[0].external_id, Some("ext-123".into()));
        assert!(matches!(txs[0].kind, TransactionKind::Income));
    }

    #[tokio::test]
    async fn handle_webhook_unknown_account_returns_zero() {
        let conn_repo = MockConnectionRepo::new();
        let tx_repo = MockTransactionRepo::new();
        let client = MockMonobankClient::empty();
        let svc = make_service(conn_repo, tx_repo, client);

        let item = MonoStatementItem {
            id: "ext-456".into(),
            time: Utc::now().timestamp(),
            description: None,
            mcc: 0,
            amount: -1000,
            operation_amount: -1000,
            currency_code: 980,
            balance: 0,
            hold: false,
        };

        let count = svc.handle_webhook("unknown-acc", &item).await.unwrap();
        assert_eq!(count, 0);
    }
}
```

**Step 2: Run the unit tests**

```bash
cargo test application::monobank 2>&1 | tail -20
```
Expected: 3 tests pass.

**Step 3: Register module in `src/application/mod.rs`**

Add:
```rust
pub mod monobank;
```

**Step 4: Run full test suite**

```bash
cargo test 2>&1 | tail -20
```
Expected: all pass.

**Step 5: Commit**

```bash
git add src/application/monobank.rs src/application/mod.rs
git commit -m "feat(app): add MonobankService with background sync and unit tests"
```

---

### Task 7: API DTOs and Handlers

**Files:**
- Modify: `src/api/dto.rs`
- Create: `src/api/handlers/monobank.rs`
- Modify: `src/api/handlers/mod.rs`

**Step 1: Add Monobank DTOs to `src/api/dto.rs`**

Append to the file:
```rust
// ── Monobank ──────────────────────────────────────────────────────────────

#[derive(Debug, serde::Deserialize)]
pub struct ConnectMonobankRequest {
    pub account_id: uuid::Uuid,
    pub token: String,
    pub monobank_account_id: String,
}

#[derive(Debug, serde::Serialize)]
pub struct MonobankConnectionResponse {
    pub id: uuid::Uuid,
    pub account_id: uuid::Uuid,
    pub monobank_account_id: String,
    pub sync_status: String,
    pub last_synced_at: Option<chrono::DateTime<chrono::Utc>>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, serde::Serialize)]
pub struct MonoAccountResponse {
    pub id: String,
    pub currency_code: u16,
    pub balance: i64,
    pub account_type: String,
    pub iban: Option<String>,
}

/// Webhook payload from Monobank
#[derive(Debug, serde::Deserialize)]
pub struct MonobankWebhookPayload {
    #[serde(rename = "type")]
    pub event_type: String,
    pub data: Option<MonobankWebhookData>,
}

#[derive(Debug, serde::Deserialize)]
pub struct MonobankWebhookData {
    pub account: String,
    #[serde(rename = "statementItem")]
    pub statement_item: crate::infrastructure::monobank_client::MonoStatementItem,
}
```

**Step 2: Create `src/api/handlers/monobank.rs`**

```rust
use axum::{
    extract::{Path, State},
    http::StatusCode,
    Extension, Json,
};
use uuid::Uuid;

use crate::api::{
    dto::{
        ConnectMonobankRequest, MonoAccountResponse, MonobankConnectionResponse,
        MonobankWebhookPayload,
    },
    error::AppError,
    middleware::AuthUser,
    state::AppState,
};
use crate::domain::monobank::{MonobankConnection, SyncStatus};

fn to_response(conn: MonobankConnection) -> MonobankConnectionResponse {
    MonobankConnectionResponse {
        id: conn.id,
        account_id: conn.account_id,
        monobank_account_id: conn.monobank_account_id,
        sync_status: conn.sync_status.as_str().to_string(),
        last_synced_at: conn.last_synced_at,
        created_at: conn.created_at,
    }
}

/// GET /monobank/client-info  (requires X-Token header)
pub async fn get_client_info(
    State(state): State<AppState>,
    Extension(AuthUser(user_id)): Extension<AuthUser>,
    axum::extract::TypedHeader(token_header): axum::extract::TypedHeader<
        axum::headers::Authorization<axum::headers::authorization::Bearer>,
    >,
) -> Result<Json<Vec<MonoAccountResponse>>, AppError> {
    let accounts = state
        .monobank
        .get_monobank_accounts(token_header.token())
        .await?;
    Ok(Json(
        accounts
            .into_iter()
            .map(|a| MonoAccountResponse {
                id: a.id,
                currency_code: a.currency_code,
                balance: a.balance,
                account_type: a.account_type,
                iban: a.iban,
            })
            .collect(),
    ))
}

/// POST /monobank/connect
pub async fn connect(
    State(state): State<AppState>,
    Extension(AuthUser(user_id)): Extension<AuthUser>,
    Json(req): Json<ConnectMonobankRequest>,
) -> Result<(StatusCode, Json<MonobankConnectionResponse>), AppError> {
    // Verify the account belongs to this user
    let (account, _) = state.accounts.get(req.account_id, user_id).await?;
    let account_created_at = account.created_at;

    let conn = state
        .monobank
        .connect(
            req.account_id,
            user_id,
            req.token,
            req.monobank_account_id,
            account_created_at,
        )
        .await?;

    Ok((StatusCode::CREATED, Json(to_response(conn))))
}

/// GET /monobank/connections
pub async fn list_connections(
    State(state): State<AppState>,
    Extension(AuthUser(user_id)): Extension<AuthUser>,
) -> Result<Json<Vec<MonobankConnectionResponse>>, AppError> {
    let conns = state.monobank.list_connections(user_id).await?;
    Ok(Json(conns.into_iter().map(to_response).collect()))
}

/// DELETE /monobank/connections/:id
pub async fn delete_connection(
    State(state): State<AppState>,
    Extension(AuthUser(user_id)): Extension<AuthUser>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, AppError> {
    state.monobank.delete_connection(id, user_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// POST /monobank/webhook  (public — no auth)
pub async fn webhook(
    State(state): State<AppState>,
    Json(payload): Json<MonobankWebhookPayload>,
) -> Result<StatusCode, AppError> {
    if payload.event_type != "StatementItem" {
        return Ok(StatusCode::OK);
    }
    if let Some(data) = payload.data {
        state
            .monobank
            .handle_webhook(&data.account, &data.statement_item)
            .await?;
    }
    Ok(StatusCode::OK)
}
```

**Note on `get_client_info`:** The `TypedHeader` approach requires `tower-http` headers. A simpler alternative — extract raw headers directly:

```rust
pub async fn get_client_info(
    State(state): State<AppState>,
    Extension(AuthUser(_user_id)): Extension<AuthUser>,
    headers: axum::http::HeaderMap,
) -> Result<Json<Vec<MonoAccountResponse>>, AppError> {
    let token = headers
        .get("X-Token")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| crate::domain::error::DomainError::InvalidInput("missing X-Token header".to_string()))?;
    let accounts = state.monobank.get_monobank_accounts(token).await?;
    Ok(Json(
        accounts
            .into_iter()
            .map(|a| MonoAccountResponse {
                id: a.id,
                currency_code: a.currency_code,
                balance: a.balance,
                account_type: a.account_type,
                iban: a.iban,
            })
            .collect(),
    ))
}
```

Use the simpler `HeaderMap` approach.

**Step 3: Register in `src/api/handlers/mod.rs`**

Add:
```rust
pub mod monobank;
```

**Step 4: Verify compiles**

```bash
cargo build 2>&1 | head -40
```
Expected: compile errors about `state.monobank` not existing yet — that's fine, fixed in Task 8.

**Step 5: Commit**

```bash
git add src/api/dto.rs src/api/handlers/monobank.rs src/api/handlers/mod.rs
git commit -m "feat(api): add Monobank DTOs and handlers"
```

---

### Task 8: Wire Up — AppState, Routes, and main.rs

**Files:**
- Modify: `src/api/state.rs`
- Modify: `src/api/routes.rs`
- Modify: `src/main.rs`

**Step 1: Add `monobank` to AppState in `src/api/state.rs`**

```rust
use std::sync::Arc;

use crate::application::accounts::AccountService;
use crate::application::auth::AuthService;
use crate::application::categories::CategoryService;
use crate::application::monobank::MonobankService;
use crate::application::transactions::TransactionService;

#[derive(Clone)]
pub struct AppState {
    pub auth: Arc<AuthService>,
    pub accounts: Arc<AccountService>,
    pub transactions: Arc<TransactionService>,
    pub categories: Arc<CategoryService>,
    pub monobank: Arc<MonobankService>,
    pub jwt_secret: String,
}
```

**Step 2: Add Monobank routes to `src/api/routes.rs`**

Add `use crate::api::handlers::monobank;` to the imports.

In the `router` function, add Monobank routes to the protected router:
```rust
.route("/monobank/client-info", get(monobank::get_client_info))
.route("/monobank/connect", post(monobank::connect))
.route("/monobank/connections", get(monobank::list_connections))
.route("/monobank/connections/{id}", delete(monobank::delete_connection))
```

And add the public webhook route (outside the protected router, before `.with_state(state)`):
```rust
Router::new()
    .nest("/auth", auth_routes)
    .merge(protected)
    .route("/monobank/webhook", post(monobank::webhook))
    .with_state(state)
```

**Step 3: Update `src/main.rs` to wire up MonobankService**

Add imports:
```rust
use moneykeeper::application::monobank::MonobankService;
use moneykeeper::infrastructure::monobank_client::ReqwestMonobankClient;
use moneykeeper::infrastructure::monobank_repository::SqliteMonobankRepository;
```

Add to the `main` function before building `state`:
```rust
let public_url = std::env::var("PUBLIC_URL")
    .unwrap_or_else(|_| "http://localhost:3000".to_string());

let monobank_service = Arc::new(MonobankService::new(
    Arc::new(SqliteMonobankRepository::new(pool.clone())),
    Arc::new(SqliteTransactionRepository::new(pool.clone())),
    Arc::new(ReqwestMonobankClient::new()),
    public_url,
));
```

Update `MonobankService::spawn_sync` — pass `public_url` into `run_sync`. Fix the webhook URL placeholder in `run_sync`:

In `src/application/monobank.rs`, change `MonobankService` to store `public_url` and pass it to `run_sync`:

```rust
// In spawn_sync:
let public_url = self.public_url.clone();

tokio::spawn(async move {
    if let Err(e) = run_sync(connection_repo, transaction_repo, monobank_client, conn, from, public_url).await {
        ...
    }
});

// In run_sync signature:
async fn run_sync(..., public_url: String) -> anyhow::Result<()> {
    ...
    let webhook_url = format!("{public_url}/monobank/webhook");
    ...
}
```

Add `monobank: monobank_service` to the `AppState` struct instantiation in `main`.

Add startup recovery after creating `monobank_service`:
```rust
monobank_service.restart_incomplete_syncs(pool.clone()).await;
```

**Step 4: Full build**

```bash
cargo build 2>&1 | head -40
```
Expected: compiles cleanly.

**Step 5: Run all tests**

```bash
cargo test 2>&1 | tail -30
```
Expected: all pass.

**Step 6: Commit**

```bash
git add src/api/state.rs src/api/routes.rs src/main.rs src/application/monobank.rs src/infrastructure/monobank_repository.rs
git commit -m "feat: wire up MonobankService into AppState, routes, and startup recovery"
```

---

### Task 9: Integration Tests

**Files:**
- Create: `tests/api/monobank.rs`
- Modify: `tests/api.rs`
- Modify: `tests/api/helpers.rs`

**Step 1: Update `tests/api/helpers.rs` — add `make_app_with_mono_client` helper**

The integration tests need a mock `MonobankApiClient`. Add to helpers:

```rust
use moneykeeper::application::monobank::MonobankService;
use moneykeeper::infrastructure::monobank_client::{MonoAccount, MonobankApiClient, MonoStatementItem};
use moneykeeper::infrastructure::monobank_repository::SqliteMonobankRepository;
use std::sync::Mutex;

pub struct MockMonobankClient {
    pub accounts: Vec<MonoAccount>,
    pub statement_items: Vec<MonoStatementItem>,
}

impl MockMonobankClient {
    pub fn empty() -> Arc<Self> {
        Arc::new(Self { accounts: vec![], statement_items: vec![] })
    }
}

#[async_trait::async_trait]
impl MonobankApiClient for MockMonobankClient {
    async fn get_accounts(&self, _token: &str) -> anyhow::Result<Vec<MonoAccount>> {
        Ok(self.accounts.clone())
    }
    async fn get_statement(&self, _token: &str, _acc: &str, _from: chrono::DateTime<chrono::Utc>, _to: chrono::DateTime<chrono::Utc>) -> anyhow::Result<Vec<MonoStatementItem>> {
        Ok(self.statement_items.clone())
    }
    async fn set_webhook(&self, _token: &str, _url: &str) -> anyhow::Result<()> { Ok(()) }
}

pub async fn make_app() -> TestServer {
    make_app_with_client(MockMonobankClient::empty()).await
}

pub async fn make_app_with_client(mono_client: Arc<dyn MonobankApiClient>) -> TestServer {
    let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
    sqlx::migrate!("src/infrastructure/migrations").run(&pool).await.unwrap();

    let tx_repo = Arc::new(SqliteTransactionRepository::new(pool.clone()));
    let state = AppState {
        auth: Arc::new(AuthService::new(Arc::new(SqliteUserRepository::new(pool.clone())))),
        accounts: Arc::new(AccountService::new(Arc::new(SqliteAccountRepository::new(pool.clone())))),
        transactions: Arc::new(TransactionService::new(tx_repo.clone())),
        categories: Arc::new(CategoryService::new(Arc::new(SqliteCategoryRepository::new(pool.clone())))),
        monobank: Arc::new(MonobankService::new(
            Arc::new(SqliteMonobankRepository::new(pool.clone())),
            tx_repo,
            mono_client,
            "http://localhost".to_string(),
        )),
        jwt_secret: "test-secret".to_string(),
    };
    TestServer::new(moneykeeper::api::routes::router(state)).unwrap()
}
```

**Step 2: Create `tests/api/monobank.rs`**

```rust
use axum::http::StatusCode;
use serde_json::Value;
use std::sync::Arc;

use super::helpers::{make_app, make_app_with_client, register_and_login, create_account_for, auth, MockMonobankClient};
use moneykeeper::infrastructure::monobank_client::{MonoAccount, MonoStatementItem};

#[tokio::test]
async fn connect_returns_201_with_pending_status() {
    let server = make_app().await;
    let (_uid, token, _) = register_and_login(&server, "a@mono.com", "pass").await;
    let account_id = create_account_for(&server, &token).await;

    let res = server
        .post("/monobank/connect")
        .add_header(auth(&token).0, auth(&token).1)
        .json(&serde_json::json!({
            "account_id": account_id,
            "token": "test-mono-token",
            "monobank_account_id": "mono-card-1"
        }))
        .await;

    assert_eq!(res.status_code(), StatusCode::CREATED);
    let body: Value = res.json();
    assert_eq!(body["sync_status"], "pending");
    assert!(body["id"].is_string());
}

#[tokio::test]
async fn list_connections_returns_created_connection() {
    let server = make_app().await;
    let (_uid, token, _) = register_and_login(&server, "b@mono.com", "pass").await;
    let account_id = create_account_for(&server, &token).await;

    server
        .post("/monobank/connect")
        .add_header(auth(&token).0, auth(&token).1)
        .json(&serde_json::json!({
            "account_id": account_id,
            "token": "tok",
            "monobank_account_id": "mono-card-2"
        }))
        .await;

    let res = server
        .get("/monobank/connections")
        .add_header(auth(&token).0, auth(&token).1)
        .await;

    assert_eq!(res.status_code(), StatusCode::OK);
    let body: Value = res.json();
    assert_eq!(body.as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn delete_connection_returns_204() {
    let server = make_app().await;
    let (_uid, token, _) = register_and_login(&server, "c@mono.com", "pass").await;
    let account_id = create_account_for(&server, &token).await;

    let conn_res = server
        .post("/monobank/connect")
        .add_header(auth(&token).0, auth(&token).1)
        .json(&serde_json::json!({
            "account_id": account_id,
            "token": "tok",
            "monobank_account_id": "mono-card-3"
        }))
        .await;

    let conn_id = conn_res.json::<Value>()["id"].as_str().unwrap().to_string();

    let res = server
        .delete(&format!("/monobank/connections/{conn_id}"))
        .add_header(auth(&token).0, auth(&token).1)
        .await;

    assert_eq!(res.status_code(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn webhook_inserts_transaction() {
    let server = make_app().await;
    let (_uid, token, _) = register_and_login(&server, "d@mono.com", "pass").await;
    let account_id = create_account_for(&server, &token).await;

    server
        .post("/monobank/connect")
        .add_header(auth(&token).0, auth(&token).1)
        .json(&serde_json::json!({
            "account_id": account_id,
            "token": "tok",
            "monobank_account_id": "mono-card-4"
        }))
        .await;

    let res = server
        .post("/monobank/webhook")
        .json(&serde_json::json!({
            "type": "StatementItem",
            "data": {
                "account": "mono-card-4",
                "statementItem": {
                    "id": "ext-tx-1",
                    "time": 1700000000,
                    "description": "Coffee",
                    "mcc": 5411,
                    "amount": -5000,
                    "operationAmount": -5000,
                    "currencyCode": 980,
                    "balance": 100000,
                    "hold": false
                }
            }
        }))
        .await;

    assert_eq!(res.status_code(), StatusCode::OK);

    // Verify the transaction was inserted
    let txs_res = server
        .get(&format!("/accounts/{account_id}/transactions"))
        .add_header(auth(&token).0, auth(&token).1)
        .await;
    let txs: Value = txs_res.json();
    assert_eq!(txs.as_array().unwrap().len(), 1);
    assert_eq!(txs[0]["kind"], "Expense");
}

#[tokio::test]
async fn webhook_duplicate_is_silently_ignored() {
    let server = make_app().await;
    let (_uid, token, _) = register_and_login(&server, "e@mono.com", "pass").await;
    let account_id = create_account_for(&server, &token).await;

    server
        .post("/monobank/connect")
        .add_header(auth(&token).0, auth(&token).1)
        .json(&serde_json::json!({
            "account_id": account_id,
            "token": "tok",
            "monobank_account_id": "mono-card-5"
        }))
        .await;

    let payload = serde_json::json!({
        "type": "StatementItem",
        "data": {
            "account": "mono-card-5",
            "statementItem": {
                "id": "ext-dup-1",
                "time": 1700000000,
                "description": "Dup",
                "mcc": 0,
                "amount": 10000,
                "operationAmount": 10000,
                "currencyCode": 980,
                "balance": 200000,
                "hold": false
            }
        }
    });

    server.post("/monobank/webhook").json(&payload).await;
    server.post("/monobank/webhook").json(&payload).await; // duplicate

    let txs_res = server
        .get(&format!("/accounts/{account_id}/transactions"))
        .add_header(auth(&token).0, auth(&token).1)
        .await;
    let txs: Value = txs_res.json();
    assert_eq!(txs.as_array().unwrap().len(), 1); // still just 1
}

#[tokio::test]
async fn get_client_info_proxies_to_monobank() {
    let client = Arc::new(MockMonobankClient {
        accounts: vec![MonoAccount {
            id: "acc-1".into(),
            currency_code: 980,
            balance: 50000,
            credit_limit: 0,
            account_type: "black".into(),
            iban: Some("UA123456789".into()),
        }],
        statement_items: vec![],
    });
    let server = make_app_with_client(client).await;
    let (_uid, token, _) = register_and_login(&server, "f@mono.com", "pass").await;

    let res = server
        .get("/monobank/client-info")
        .add_header(auth(&token).0, auth(&token).1)
        .add_header(
            axum::http::header::HeaderName::from_static("x-token"),
            "my-mono-token".parse().unwrap(),
        )
        .await;

    assert_eq!(res.status_code(), StatusCode::OK);
    let body: Value = res.json();
    assert_eq!(body[0]["id"], "acc-1");
}
```

**Step 3: Register in `tests/api.rs`**

Add:
```rust
#[path = "api/monobank.rs"]
mod monobank;
```

**Step 4: Run integration tests**

```bash
cargo test --test api 2>&1 | tail -30
```
Expected: all tests pass including the new Monobank ones.

**Step 5: Run full lint + format**

```bash
cargo clippy -- -D warnings 2>&1 | head -40
cargo fmt --check
```
Fix any issues.

**Step 6: Commit**

```bash
git add tests/api/monobank.rs tests/api.rs tests/api/helpers.rs
git commit -m "test: add Monobank integration tests"
```

---

### Task 10: Final Verification

**Step 1: Run all tests**

```bash
cargo test 2>&1 | tail -20
```
Expected: all pass.

**Step 2: Run clippy**

```bash
cargo clippy -- -D warnings
```
Expected: no warnings.

**Step 3: Format check**

```bash
cargo fmt --check
```
Expected: no diffs.

**Step 4: Update `.env` with PUBLIC_URL**

Add to `.env`:
```
PUBLIC_URL=https://your-public-host.com
```

**Step 5: Final commit**

```bash
git add .env
git commit -m "chore: add PUBLIC_URL to .env for Monobank webhook registration"
```
