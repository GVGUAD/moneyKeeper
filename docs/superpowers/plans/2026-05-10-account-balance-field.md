# Account Balance Field Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Store a `balance` column on the `accounts` table, updated atomically on every transaction mutation, so every account response includes an up-to-date balance without a separate endpoint.

**Architecture:** A new `balance NUMERIC NOT NULL DEFAULT 0` column is added via migration. The `AccountRepository` trait gains `adjust_balance` (replaces `compute_balance`). Every path that creates or deletes a transaction — `TransactionService`, `MonobankService` — calls `adjust_balance` with the signed delta. `AccountResponse` gains a `balance` field; the dedicated `GET /accounts/{id}/balance` endpoint is removed.

**Tech Stack:** Rust, sqlx (PostgreSQL), axum, tokio, rust_decimal

---

## File Map

| File | Action |
|------|--------|
| `src/infrastructure/migrations/0005_account_balance.sql` | Create — add `balance` column |
| `src/domain/account.rs` | Modify — add `balance` to `Account`, swap trait method |
| `src/domain/transaction.rs` | Modify — `create_idempotent` returns `bool` |
| `src/infrastructure/account_repository.rs` | Modify — `AccountRow`, queries, remove `compute_balance`, add `adjust_balance` |
| `src/infrastructure/transaction_repository.rs` | Modify — `create_idempotent` returns `bool` |
| `src/application/transactions.rs` | Modify — add `account_repo` dep, balance deltas in `create`/`delete` |
| `src/application/accounts.rs` | Modify — remove `get_balance` |
| `src/application/monobank.rs` | Modify — add `account_repo`, adjust balance after idempotent insert |
| `src/api/dto.rs` | Modify — add `balance` to `AccountResponse`, remove `BalanceResponse` |
| `src/api/handlers/accounts.rs` | Modify — add `balance` to mapping, remove `get_balance` handler |
| `src/api/routes.rs` | Modify — remove `/accounts/{id}/balance` route |
| `src/main.rs` | Modify — rewire service constructors |
| `static/openapi.json` | Modify — update schemas, remove balance path |

---

## Task 1: DB Migration

**Files:**
- Create: `src/infrastructure/migrations/0005_account_balance.sql`

- [ ] **Step 1: Create the migration file**

```sql
ALTER TABLE accounts ADD COLUMN balance NUMERIC NOT NULL DEFAULT 0;
```

- [ ] **Step 2: Verify it compiles with sqlx's migration macro**

Run:
```bash
cargo test --test '*' 2>&1 | head -30
```
Expected: tests that use `migrations = "src/infrastructure/migrations"` pick up the new file without errors. If you see `sqlx::migrate!` errors, check the file is numbered `0005_` and ends in `.sql`.

- [ ] **Step 3: Commit**

```bash
git add src/infrastructure/migrations/0005_account_balance.sql
git commit -m "feat: add balance column to accounts table"
```

---

## Task 2: Domain — `Account` struct and `AccountRepository` trait

**Files:**
- Modify: `src/domain/account.rs`

- [ ] **Step 1: Write a failing test for balance initialization**

Add inside the existing `#[cfg(test)] mod tests` block at the bottom of `src/domain/account.rs`:

```rust
#[test]
fn account_new_has_zero_balance() {
    let acct = Account::new(
        Uuid::new_v4(),
        "Wallet".to_string(),
        AccountType::Cash,
        "USD".to_string(),
    );
    assert_eq!(acct.balance, Decimal::ZERO);
}
```

- [ ] **Step 2: Run the test to verify it fails**

```bash
cargo test account_new_has_zero_balance
```
Expected: compile error — `balance` field doesn't exist yet.

- [ ] **Step 3: Add `balance` field to `Account` and initialize it in `new()`**

In `src/domain/account.rs`, replace the `Account` struct (lines 41–50) and its `new()` method (lines 52–65):

```rust
#[derive(Debug, Clone)]
pub struct Account {
    pub id: Uuid,
    pub user_id: Uuid,
    pub name: String,
    pub account_type: AccountType,
    pub currency: String,
    pub balance: Decimal,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Account {
    pub fn new(user_id: Uuid, name: String, account_type: AccountType, currency: String) -> Self {
        let now = Utc::now();
        Self {
            id: Uuid::new_v4(),
            user_id,
            name,
            account_type,
            currency,
            balance: Decimal::ZERO,
            created_at: now,
            updated_at: now,
        }
    }
}
```

- [ ] **Step 4: Replace `compute_balance` with `adjust_balance` on the trait**

In `src/domain/account.rs`, replace the `AccountRepository` trait (lines 162–174):

```rust
#[async_trait::async_trait]
pub trait AccountRepository: Send + Sync {
    async fn create(&self, account: &Account, details: &AccountDetails) -> anyhow::Result<()>;
    async fn find_by_id(
        &self,
        id: Uuid,
        user_id: Uuid,
    ) -> anyhow::Result<Option<(Account, AccountDetails)>>;
    async fn list_by_user(&self, user_id: Uuid) -> anyhow::Result<Vec<(Account, AccountDetails)>>;
    async fn update(&self, account: &Account, details: &AccountDetails) -> anyhow::Result<()>;
    async fn delete(&self, id: Uuid, user_id: Uuid) -> anyhow::Result<()>;
    async fn adjust_balance(
        &self,
        account_id: Uuid,
        user_id: Uuid,
        delta: Decimal,
    ) -> anyhow::Result<()>;
}
```

- [ ] **Step 5: Run tests**

```bash
cargo test
```
Expected: compile errors in `account_repository.rs` (infra) and `accounts.rs` (application) because they still reference `compute_balance`. That's fine — we fix those in subsequent tasks.

- [ ] **Step 6: Commit**

```bash
git add src/domain/account.rs
git commit -m "feat: add balance field to Account and adjust_balance to AccountRepository trait"
```

---

## Task 3: Domain — `create_idempotent` returns `bool`

**Files:**
- Modify: `src/domain/transaction.rs`

- [ ] **Step 1: Update the trait signature**

In `src/domain/transaction.rs`, replace line 136:

```rust
    /// Insert a transaction using INSERT OR IGNORE (for external syncs).
    /// Returns true if the row was actually inserted (false = already existed).
    async fn create_idempotent(&self, tx: &Transaction) -> anyhow::Result<bool>;
```

- [ ] **Step 2: Run tests to see what breaks**

```bash
cargo test 2>&1 | grep "error"
```
Expected: errors in `transaction_repository.rs` (impl) and `monobank.rs` (caller). Fix in Tasks 4 and 7.

- [ ] **Step 3: Commit**

```bash
git add src/domain/transaction.rs
git commit -m "feat: create_idempotent returns bool (was_inserted) for balance tracking"
```

---

## Task 4: Infrastructure — `SqliteAccountRepository`

**Files:**
- Modify: `src/infrastructure/account_repository.rs`

- [ ] **Step 1: Write a failing test for balance**

Add inside the `#[cfg(test)] mod tests` block in `src/infrastructure/account_repository.rs`:

```rust
#[sqlx::test(migrations = "src/infrastructure/migrations")]
async fn new_account_has_zero_balance(pool: PgPool) {
    let repo = SqliteAccountRepository::new(pool);
    let user_id = Uuid::new_v4();
    let account = Account::new(
        user_id,
        "Savings".to_string(),
        AccountType::Cash,
        "USD".to_string(),
    );
    repo.create(&account, &AccountDetails::None).await.unwrap();
    let (found, _) = repo.find_by_id(account.id, user_id).await.unwrap().unwrap();
    assert_eq!(found.balance, Decimal::ZERO);
}

#[sqlx::test(migrations = "src/infrastructure/migrations")]
async fn adjust_balance_adds_delta(pool: PgPool) {
    let repo = SqliteAccountRepository::new(pool);
    let user_id = Uuid::new_v4();
    let account = Account::new(
        user_id,
        "Cash".to_string(),
        AccountType::Cash,
        "USD".to_string(),
    );
    repo.create(&account, &AccountDetails::None).await.unwrap();
    repo.adjust_balance(account.id, user_id, Decimal::new(100, 0)).await.unwrap();
    repo.adjust_balance(account.id, user_id, Decimal::new(-30, 0)).await.unwrap();
    let (found, _) = repo.find_by_id(account.id, user_id).await.unwrap().unwrap();
    assert_eq!(found.balance, Decimal::new(70, 0));
}
```

- [ ] **Step 2: Run tests to confirm they fail**

```bash
cargo test new_account_has_zero_balance adjust_balance_adds_delta
```
Expected: compile errors — `AccountRow` missing `balance`, `adjust_balance` not implemented.

- [ ] **Step 3: Update `AccountRow` and `row_to_account`**

In `src/infrastructure/account_repository.rs`, replace `AccountRow` (lines 23–32) and `row_to_account` (lines 34–44):

```rust
#[derive(sqlx::FromRow)]
struct AccountRow {
    id: Uuid,
    user_id: Uuid,
    name: String,
    account_type: String,
    currency: String,
    balance: Decimal,
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
        balance: r.balance,
        created_at: r.created_at,
        updated_at: r.updated_at,
    })
}
```

- [ ] **Step 4: Remove `compute_balance`, add `adjust_balance` to the impl block**

In `src/infrastructure/account_repository.rs`, delete the `compute_balance` method (lines 138–157) and add `adjust_balance` in its place inside the `impl AccountRepository for SqliteAccountRepository` block:

```rust
    async fn adjust_balance(
        &self,
        account_id: Uuid,
        user_id: Uuid,
        delta: Decimal,
    ) -> anyhow::Result<()> {
        sqlx::query(
            "UPDATE accounts SET balance = balance + $1 WHERE id = $2 AND user_id = $3",
        )
        .bind(delta)
        .bind(account_id)
        .bind(user_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
```

Also remove the `use crate::domain::transaction::TransactionKind;` import at line 11 — it was only used by `compute_balance`.

- [ ] **Step 5: Run the new tests**

```bash
cargo test new_account_has_zero_balance adjust_balance_adds_delta
```
Expected: PASS.

- [ ] **Step 6: Run all tests**

```bash
cargo test
```
Expected: only remaining failures are in files that haven't been updated yet (application/accounts.rs, application/transactions.rs, application/monobank.rs). No panic in infrastructure tests.

- [ ] **Step 7: Commit**

```bash
git add src/infrastructure/account_repository.rs
git commit -m "feat: update SqliteAccountRepository with balance field and adjust_balance"
```

---

## Task 5: Infrastructure — `create_idempotent` returns `bool`

**Files:**
- Modify: `src/infrastructure/transaction_repository.rs`

- [ ] **Step 1: Update the implementation**

In `src/infrastructure/transaction_repository.rs`, replace the `create_idempotent` method (lines 250–272):

```rust
    async fn create_idempotent(&self, tx: &Transaction) -> anyhow::Result<bool> {
        let result = sqlx::query(
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
        Ok(result.rows_affected() > 0)
    }
```

- [ ] **Step 2: Update the `MockTransactionRepo` in `src/application/monobank.rs` tests**

The mock's `create_idempotent` returns `Ok(())` today — after this step it must return `Ok(bool)`. We fix this in Task 7 together with the rest of monobank changes.

- [ ] **Step 3: Verify only monobank is still broken**

```bash
cargo test 2>&1 | grep "error\[" | grep -v "monobank"
```
Expected: no non-monobank compile errors.

- [ ] **Step 4: Commit**

```bash
git add src/infrastructure/transaction_repository.rs
git commit -m "feat: create_idempotent returns bool indicating whether row was inserted"
```

---

## Task 6: Application — `AccountService` (remove `get_balance`)

**Files:**
- Modify: `src/application/accounts.rs`

- [ ] **Step 1: Remove `get_balance` and the `Decimal` import**

In `src/application/accounts.rs`, delete lines 2 (`use rust_decimal::Decimal;`) and lines 68–71 (the `get_balance` method):

```rust
use chrono::Utc;
use std::sync::Arc;
use uuid::Uuid;

use crate::domain::account::{Account, AccountDetails, AccountRepository, AccountType};
use crate::domain::error::DomainError;

pub struct AccountService {
    repo: Arc<dyn AccountRepository>,
}

impl AccountService {
    pub fn new(repo: Arc<dyn AccountRepository>) -> Self {
        Self { repo }
    }

    pub async fn create(
        &self,
        user_id: Uuid,
        name: String,
        account_type: AccountType,
        currency: String,
        details: AccountDetails,
    ) -> anyhow::Result<Account> {
        let account = Account::new(user_id, name, account_type, currency);
        self.repo.create(&account, &details).await?;
        Ok(account)
    }

    pub async fn get(&self, id: Uuid, user_id: Uuid) -> anyhow::Result<(Account, AccountDetails)> {
        self.repo
            .find_by_id(id, user_id)
            .await?
            .ok_or_else(|| DomainError::NotFound(format!("account {id}")).into())
    }

    pub async fn list(&self, user_id: Uuid) -> anyhow::Result<Vec<(Account, AccountDetails)>> {
        self.repo.list_by_user(user_id).await
    }

    pub async fn update(
        &self,
        id: Uuid,
        user_id: Uuid,
        name: Option<String>,
        currency: Option<String>,
        details: Option<AccountDetails>,
    ) -> anyhow::Result<(Account, AccountDetails)> {
        let (mut account, existing_details) = self.get(id, user_id).await?;
        if let Some(n) = name {
            account.name = n;
        }
        if let Some(c) = currency {
            account.currency = c;
        }
        account.updated_at = Utc::now();
        let new_details = details.unwrap_or(existing_details);
        self.repo.update(&account, &new_details).await?;
        Ok((account, new_details))
    }

    pub async fn delete(&self, id: Uuid, user_id: Uuid) -> anyhow::Result<()> {
        self.get(id, user_id).await?;
        self.repo.delete(id, user_id).await
    }
}
```

- [ ] **Step 2: Run tests**

```bash
cargo test
```
Expected: only monobank and handlers/routes still broken.

- [ ] **Step 3: Commit**

```bash
git add src/application/accounts.rs
git commit -m "feat: remove get_balance from AccountService"
```

---

## Task 7: Application — `TransactionService` (add balance delta)

**Files:**
- Modify: `src/application/transactions.rs`

- [ ] **Step 1: Update the file**

Replace the entire `src/application/transactions.rs` with:

```rust
use chrono::DateTime;
use chrono::Utc;
use rust_decimal::Decimal;
use std::sync::Arc;
use uuid::Uuid;

use crate::domain::account::AccountRepository;
use crate::domain::error::DomainError;
use crate::domain::transaction::{
    Transaction, TransactionDetails, TransactionKind, TransactionListParams, TransactionRepository,
};

fn signed_delta(kind: &TransactionKind, amount: Decimal) -> Decimal {
    if kind.affects_balance_positively() {
        amount
    } else {
        -amount
    }
}

pub struct TransactionService {
    repo: Arc<dyn TransactionRepository>,
    account_repo: Arc<dyn AccountRepository>,
}

impl TransactionService {
    pub fn new(
        repo: Arc<dyn TransactionRepository>,
        account_repo: Arc<dyn AccountRepository>,
    ) -> Self {
        Self { repo, account_repo }
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn create(
        &self,
        account_id: Uuid,
        user_id: Uuid,
        amount: Decimal,
        currency: String,
        kind: TransactionKind,
        category_id: Option<Uuid>,
        note: Option<String>,
        transacted_at: DateTime<Utc>,
        details: TransactionDetails,
    ) -> anyhow::Result<Transaction> {
        if amount <= Decimal::ZERO {
            return Err(DomainError::InvalidInput("amount must be positive".to_string()).into());
        }
        let tx = Transaction::new(
            account_id,
            user_id,
            amount,
            currency,
            kind.clone(),
            category_id,
            note,
            transacted_at,
        );
        self.repo.create(&tx, &details).await?;
        self.account_repo
            .adjust_balance(account_id, user_id, signed_delta(&kind, amount))
            .await?;
        Ok(tx)
    }

    pub async fn get(
        &self,
        id: Uuid,
        user_id: Uuid,
    ) -> anyhow::Result<(Transaction, TransactionDetails)> {
        self.repo
            .find_by_id(id, user_id)
            .await?
            .ok_or_else(|| DomainError::NotFound(format!("transaction {id}")).into())
    }

    pub async fn list(
        &self,
        params: TransactionListParams,
    ) -> anyhow::Result<Vec<(Transaction, TransactionDetails)>> {
        self.repo.list(&params).await
    }

    pub async fn delete(&self, id: Uuid, user_id: Uuid) -> anyhow::Result<()> {
        let (tx, _) = self.get(id, user_id).await?;
        self.repo.delete(id, user_id).await?;
        self.account_repo
            .adjust_balance(
                tx.account_id,
                tx.user_id,
                -signed_delta(&tx.kind, tx.amount),
            )
            .await?;
        Ok(())
    }
}
```

- [ ] **Step 2: Run tests**

```bash
cargo test
```
Expected: `TransactionService::new` call sites in `main.rs` and handler tests fail to compile (wrong number of args). Still OK — we fix main.rs in Task 9.

- [ ] **Step 3: Commit**

```bash
git add src/application/transactions.rs
git commit -m "feat: TransactionService adjusts account balance on create/delete"
```

---

## Task 8: Application — `MonobankService` (balance on sync)

**Files:**
- Modify: `src/application/monobank.rs`

- [ ] **Step 1: Add `account_repo` field and update `new()`**

In `src/application/monobank.rs`, replace the struct definition and imports at the top:

```rust
use std::sync::Arc;

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use uuid::Uuid;

use crate::domain::account::AccountRepository;
use crate::domain::bank_connection::{
    BankConnection, BankConnectionRepository, BankProvider, SyncStatus,
};
use crate::domain::error::DomainError;
use crate::domain::monobank::{MonoAccount, MonoStatementItem, MonobankApiClient};
use crate::domain::transaction::{Transaction, TransactionKind, TransactionRepository};
```

Replace the `MonobankService` struct and `new()`:

```rust
pub struct MonobankService {
    connection_repo: Arc<dyn BankConnectionRepository>,
    transaction_repo: Arc<dyn TransactionRepository>,
    account_repo: Arc<dyn AccountRepository>,
    monobank_client: Arc<dyn MonobankApiClient>,
    public_url: String,
}

impl MonobankService {
    pub fn new(
        connection_repo: Arc<dyn BankConnectionRepository>,
        transaction_repo: Arc<dyn TransactionRepository>,
        account_repo: Arc<dyn AccountRepository>,
        monobank_client: Arc<dyn MonobankApiClient>,
        public_url: String,
    ) -> Self {
        Self {
            connection_repo,
            transaction_repo,
            account_repo,
            monobank_client,
            public_url,
        }
    }
```

- [ ] **Step 2: Update `spawn_sync` to pass `account_repo`**

Replace the `spawn_sync` method:

```rust
    pub fn spawn_sync(&self, conn: BankConnection, from: DateTime<Utc>) {
        let connection_repo = Arc::clone(&self.connection_repo);
        let transaction_repo = Arc::clone(&self.transaction_repo);
        let account_repo = Arc::clone(&self.account_repo);
        let monobank_client = Arc::clone(&self.monobank_client);
        let public_url = self.public_url.clone();

        tokio::spawn(async move {
            run_sync(
                connection_repo,
                transaction_repo,
                account_repo,
                monobank_client,
                conn,
                from,
                public_url,
            )
            .await;
        });
    }
```

- [ ] **Step 3: Update `insert_statement_item` to return `bool` and adjust balance**

Replace the `insert_statement_item` method:

```rust
    async fn insert_statement_item(
        &self,
        conn: &BankConnection,
        item: &MonoStatementItem,
    ) -> anyhow::Result<bool> {
        let tx = build_transaction(conn.account_id, conn.user_id, item);
        let inserted = self.transaction_repo.create_idempotent(&tx).await?;
        if inserted {
            let delta = if tx.kind.affects_balance_positively() {
                tx.amount
            } else {
                -tx.amount
            };
            self.account_repo
                .adjust_balance(tx.account_id, tx.user_id, delta)
                .await?;
        }
        Ok(inserted)
    }
```

- [ ] **Step 4: Update the free `run_sync` function signature and body**

Replace the `run_sync` function:

```rust
async fn run_sync(
    connection_repo: Arc<dyn BankConnectionRepository>,
    transaction_repo: Arc<dyn TransactionRepository>,
    account_repo: Arc<dyn AccountRepository>,
    monobank_client: Arc<dyn MonobankApiClient>,
    conn: BankConnection,
    history_from: DateTime<Utc>,
    public_url: String,
) {
    if let Err(e) = connection_repo
        .update_status(conn.id, SyncStatus::Syncing, None)
        .await
    {
        tracing::error!(conn_id = %conn.id, "failed to set sync status to Syncing: {e}");
        return;
    }
    tracing::info!(conn_id = %conn.id, "starting monobank sync");

    let now = Utc::now();
    let mut cursor = history_from;

    while cursor < now {
        let to = (cursor + chrono::Duration::days(31)).min(now);

        let items = match monobank_client
            .get_statement(&conn.token, &conn.external_account_id, cursor, to)
            .await
        {
            Ok(items) => items,
            Err(e) => {
                tracing::error!(conn_id = %conn.id, "failed to fetch monobank statement: {e}");
                if let Err(e2) = connection_repo
                    .update_status(conn.id, SyncStatus::Failed, None)
                    .await
                {
                    tracing::error!(conn_id = %conn.id, "failed to set sync status to Failed: {e2}");
                }
                return;
            }
        };
        tracing::info!("in the loop {}", items.len());
        for item in &items {
            let tx = build_transaction(conn.account_id, conn.user_id, item);
            match transaction_repo.create_idempotent(&tx).await {
                Ok(true) => {
                    let delta = if tx.kind.affects_balance_positively() {
                        tx.amount
                    } else {
                        -tx.amount
                    };
                    if let Err(e) = account_repo
                        .adjust_balance(tx.account_id, tx.user_id, delta)
                        .await
                    {
                        tracing::error!(
                            conn_id = %conn.id,
                            item_id = %item.id,
                            "failed to adjust account balance: {e}"
                        );
                    }
                }
                Ok(false) => {}
                Err(e) => {
                    tracing::error!(
                        conn_id = %conn.id,
                        item_id = %item.id,
                        "failed to insert statement item: {e}"
                    );
                }
            }
        }

        cursor = to;
        if cursor < now {
            tokio::time::sleep(tokio::time::Duration::from_secs(61)).await;
        }
    }
    tracing::info!(conn_id = %conn.id, "monobank sync completed");
    if let Err(e) = connection_repo
        .update_status(conn.id, SyncStatus::Completed, Some(Utc::now()))
        .await
    {
        tracing::error!(conn_id = %conn.id, "failed to set sync status to Completed: {e}");
    }
}
```

- [ ] **Step 5: Update the test mock to return `bool` from `create_idempotent`**

In the `#[cfg(test)] mod tests` block inside `src/application/monobank.rs`, find `MockTransactionRepo` and update `create_idempotent`:

```rust
        async fn create_idempotent(&self, tx: &Transaction) -> anyhow::Result<bool> {
            let mut txs = self.transactions.lock().unwrap();
            let already_exists = tx.external_id.as_ref().is_some_and(|eid| {
                txs.iter()
                    .any(|t| t.external_id.as_deref() == Some(eid.as_str()))
            });
            if !already_exists {
                txs.push(tx.clone());
                Ok(true)
            } else {
                Ok(false)
            }
        }
```

Also update the `make_service` helper to add a mock `account_repo`:

```rust
    struct MockAccountRepo;

    #[async_trait::async_trait]
    impl crate::domain::account::AccountRepository for MockAccountRepo {
        async fn create(
            &self,
            _account: &crate::domain::account::Account,
            _details: &crate::domain::account::AccountDetails,
        ) -> anyhow::Result<()> { Ok(()) }
        async fn find_by_id(
            &self,
            _id: Uuid,
            _user_id: Uuid,
        ) -> anyhow::Result<Option<(crate::domain::account::Account, crate::domain::account::AccountDetails)>> {
            Ok(None)
        }
        async fn list_by_user(
            &self,
            _user_id: Uuid,
        ) -> anyhow::Result<Vec<(crate::domain::account::Account, crate::domain::account::AccountDetails)>> {
            Ok(vec![])
        }
        async fn update(
            &self,
            _account: &crate::domain::account::Account,
            _details: &crate::domain::account::AccountDetails,
        ) -> anyhow::Result<()> { Ok(()) }
        async fn delete(&self, _id: Uuid, _user_id: Uuid) -> anyhow::Result<()> { Ok(()) }
        async fn adjust_balance(
            &self,
            _account_id: Uuid,
            _user_id: Uuid,
            _delta: rust_decimal::Decimal,
        ) -> anyhow::Result<()> { Ok(()) }
    }

    fn make_service(
        conn_repo: Arc<dyn BankConnectionRepository>,
        tx_repo: Arc<dyn TransactionRepository>,
    ) -> MonobankService {
        MonobankService::new(
            conn_repo,
            tx_repo,
            Arc::new(MockAccountRepo),
            Arc::new(MockMonobankClient),
            "https://example.com".to_string(),
        )
    }
```

- [ ] **Step 6: Run monobank tests**

```bash
cargo test --lib monobank
```
Expected: all existing monobank tests pass.

- [ ] **Step 7: Run all tests**

```bash
cargo test
```
Expected: only `main.rs` wiring and handler compile errors remain.

- [ ] **Step 8: Commit**

```bash
git add src/application/monobank.rs
git commit -m "feat: MonobankService adjusts account balance when syncing transactions"
```

---

## Task 9: API — DTO, handlers, routes, and `main.rs`

**Files:**
- Modify: `src/api/dto.rs`
- Modify: `src/api/handlers/accounts.rs`
- Modify: `src/api/routes.rs`
- Modify: `src/main.rs`

- [ ] **Step 1: Update `AccountResponse` DTO and remove `BalanceResponse`**

In `src/api/dto.rs`, replace lines 84–100:

```rust
#[derive(Serialize)]
pub struct AccountResponse {
    pub id: Uuid,
    pub name: String,
    pub account_type: String,
    pub currency: String,
    pub balance: Decimal,
    pub details: Option<AccountDetailsDto>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
```

(Delete the `BalanceResponse` struct entirely.)

- [ ] **Step 2: Update handler file**

Replace `src/api/handlers/accounts.rs` imports and functions:

Remove `BalanceResponse` from the import on line 7–9:
```rust
use crate::api::dto::{
    AccountDetailsDto, AccountResponse, CreateAccountRequest, UpdateAccountRequest,
};
```

Update `account_to_response` (lines 91–101) to include `balance`:
```rust
fn account_to_response(a: Account, d: AccountDetails) -> AccountResponse {
    AccountResponse {
        id: a.id,
        name: a.name,
        account_type: a.account_type.as_str().to_string(),
        currency: a.currency,
        balance: a.balance,
        details: details_to_dto(&d),
        created_at: a.created_at,
        updated_at: a.updated_at,
    }
}
```

Delete the entire `get_balance` function (lines 172–184).

- [ ] **Step 3: Remove the balance route**

In `src/api/routes.rs`, delete line 34:
```rust
        .route("/accounts/{id}/balance", get(accounts::get_balance))
```

- [ ] **Step 4: Rewire `main.rs`**

Replace the service construction block in `src/main.rs` (lines 39–58). Use explicit trait-object type annotations on the `let` bindings so `Arc::clone` carries the right type automatically:

```rust
    use moneykeeper::domain::account::AccountRepository;
    use moneykeeper::domain::transaction::TransactionRepository;

    let account_repo: Arc<dyn AccountRepository> =
        Arc::new(SqliteAccountRepository::new(pool.clone()));
    let transaction_repo: Arc<dyn TransactionRepository> =
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
```

- [ ] **Step 5: Build the project**

```bash
cargo build
```
Expected: clean compile.

- [ ] **Step 6: Run all tests**

```bash
cargo test
```
Expected: all tests pass.

- [ ] **Step 7: Commit**

```bash
git add src/api/dto.rs src/api/handlers/accounts.rs src/api/routes.rs src/main.rs
git commit -m "feat: include balance in AccountResponse, remove /accounts/{id}/balance endpoint"
```

---

## Task 10: OpenAPI Spec

**Files:**
- Modify: `static/openapi.json`

- [ ] **Step 1: Add `balance` to `AccountResponse` schema**

In `static/openapi.json`, find the `AccountResponse` schema in `components/schemas`. Add `"balance"` to the `required` array and add the property:

```json
"balance": {
  "type": "number",
  "description": "Current account balance"
}
```

The `required` array should become:
```json
"required": ["id", "name", "account_type", "currency", "balance", "created_at", "updated_at"]
```

- [ ] **Step 2: Remove the `/accounts/{id}/balance` path**

In `static/openapi.json`, find and delete the entire path entry for `/accounts/{id}/balance` (the key and its value object).

- [ ] **Step 3: Remove `BalanceResponse` from `components/schemas`**

Find and delete the `BalanceResponse` schema entry from `components/schemas`.

- [ ] **Step 4: Verify the JSON is valid**

```bash
python3 -c "import json, sys; json.load(open('static/openapi.json')); print('valid JSON')"
```
Expected: `valid JSON`

- [ ] **Step 5: Build to confirm the embedded spec compiles**

```bash
cargo build
```
Expected: clean compile (the spec is embedded via `include_str!`).

- [ ] **Step 6: Commit**

```bash
git add static/openapi.json
git commit -m "docs: update OpenAPI spec — add balance to AccountResponse, remove balance endpoint"
```

---

## Task 11: End-to-end verification

- [ ] **Step 1: Run the full test suite**

```bash
cargo test
```
Expected: all tests pass, zero warnings from `cargo clippy`.

- [ ] **Step 2: Lint**

```bash
cargo clippy -- -D warnings
```
Expected: clean.

- [ ] **Step 3: Confirm balance appears in account response**

```bash
cargo run &
sleep 2
# Create an account (replace TOKEN with a valid Supabase JWT)
curl -s -X POST http://localhost:8080/accounts \
  -H "Authorization: Bearer TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"name":"Test","account_type":"Cash","currency":"USD"}' | jq .balance
```
Expected: `0`

- [ ] **Step 4: Confirm balance endpoint returns 404**

```bash
curl -s -o /dev/null -w "%{http_code}" \
  http://localhost:8080/accounts/any-id/balance \
  -H "Authorization: Bearer TOKEN"
```
Expected: `404` (route no longer exists).

- [ ] **Step 5: Final commit (if any cleanup was needed)**

```bash
git add -p
git commit -m "chore: final cleanup after balance field implementation"
```
