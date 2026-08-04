# Stats & Graphs Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add five `/stats` endpoints (dashboard, balance-history, cashflow, categories, investments) with FX-rate-normalized aggregations to a user-configurable base currency.

**Architecture:** DDD layout (domain → application → infrastructure → api). New `fx_rates` and `user_settings` tables. NBU as FX source via a daily tokio background task. Aggregations are query-time SQL with a rate-as-of-date CTE. Cost-basis investment math (average cost) lives in the service layer.

**Tech Stack:** Rust 2024, axum 0.8, sqlx 0.8 (postgres), tokio, reqwest, rust_decimal, uuid, chrono.

**Reference spec:** `docs/superpowers/specs/2026-05-10-stats-and-graphs-design.md`

---

## File Structure

**Migrations (created):**
- `src/infrastructure/migrations/0006_fx_rates.sql` — `fx_rates` table.
- `src/infrastructure/migrations/0007_user_settings.sql` — `user_settings` table.

**Domain (created):**
- `src/domain/fx_rate.rs` — `FxRate` value object, `FxRateRepository` trait, `FxRateSource` trait.
- `src/domain/user_settings.rs` — `UserSettings` entity, `UserSettingsRepository` trait.
- `src/domain/stats.rs` — Result types (`DashboardStats`, `BalanceHistoryPoint`, `CashflowPoint`, `CategoryBreakdownItem`, `TickerHolding`, `Granularity`, `MissingRate`) and `StatsRepository` trait.

**Domain (modified):**
- `src/domain/mod.rs` — add `pub mod fx_rate;`, `pub mod user_settings;`, `pub mod stats;`.

**Application (created):**
- `src/application/stats.rs` — `StatsService` (resolves base currency, calls repo, computes investments avg-cost in code).
- `src/application/fx_sync.rs` — `FxSyncUseCase` (`sync_today`, `backfill`).
- `src/application/user_settings.rs` — `UserSettingsService`.

**Application (modified):**
- `src/application/mod.rs` — register new modules.

**Infrastructure (created):**
- `src/infrastructure/fx_rate_repository.rs` — `PgFxRateRepository`.
- `src/infrastructure/user_settings_repository.rs` — `PgUserSettingsRepository`.
- `src/infrastructure/stats_repository.rs` — `PgStatsRepository` (one method per endpoint).
- `src/infrastructure/nbu_client.rs` — `NbuFxRateSource` HTTP client.

**Infrastructure (modified):**
- `src/infrastructure/mod.rs` — register new modules.

**API (created):**
- `src/api/handlers/stats.rs` — five handlers.
- `src/api/handlers/user_settings.rs` — GET/PATCH `/me/settings`.

**API (modified):**
- `src/api/handlers/mod.rs` — register new modules.
- `src/api/dto.rs` — add response types: `DashboardResponse`, `BalanceHistoryResponse`, `CashflowResponse`, `CategoriesResponse`, `InvestmentsResponse`, `UserSettingsResponse`, `UpdateUserSettingsRequest`, `MissingRateDto`, `StatsRangeQuery`, `StatsGranularityQuery`, `CategoriesQuery`, `DashboardQuery`.
- `src/api/routes.rs` — register new routes.
- `src/api/state.rs` — add `stats: Arc<StatsService>`, `user_settings: Arc<UserSettingsService>`.

**Main (modified):**
- `src/main.rs` — wire new services, spawn FX sync background task.

**Tests (created):**
- `tests/api/stats.rs` — integration tests for all five stats endpoints + user settings.
- `tests/api/fx_rates.rs` — integration tests for FX repo + sync use case.

**Tests (modified):**
- `tests/api/helpers.rs` — helpers for seeding `fx_rates` and `user_settings`.
- `tests/api/mod.rs` — register new test modules.

---

## Investigation Step (Task 0)

Before any code, verify how Transfer transactions are stored — the spec calls this out.

### Task 0: Verify Transfer sign convention

**Files:**
- Read: `src/application/transactions.rs`, `src/infrastructure/transaction_repository.rs`, `src/application/monobank.rs`, `src/api/handlers/transactions.rs`

- [ ] **Step 1: Trace what happens when a Transfer transaction is created**

Search for callers that create Transfer transactions:

```bash
rg -n "TransactionKind::Transfer|kind = .Transfer.|\"Transfer\"" src/
```

- [ ] **Step 2: Confirm sign rule**

Identify whether:
- (a) Two `Transaction` rows are stored (one per account) and `transfer_links` joins them, OR
- (b) One row plus a `transfer_links` row indicating destination, OR
- (c) Two rows with opposite signs already encoded in `amount`.

Existing `signed_delta` in `src/application/transactions.rs:13-19` uses
`affects_balance_positively`, which returns `false` for Transfer — so a
Transfer row currently subtracts from its account's balance. That implies
either (a) with `from`-side stored only, or that the destination side is
created with a different `kind`.

- [ ] **Step 3: Document findings inline at the top of `src/infrastructure/stats_repository.rs` when you create it**

Write a short comment like:

```rust
// Transfer convention (verified <date>):
// - <findings>
// - balance-history aggregation handles Transfer as: <rule>
```

- [ ] **Step 4: Pick the aggregation rule**

Two safe options that work regardless of how Transfer is stored:

- **Option A:** Exclude `kind = 'Transfer'` from balance-history and
  cashflow entirely. Net worth from Income/Expense/Buy/Sell/StakingReward
  + initial balances. Simplest.
- **Option B:** Mirror `signed_delta`: treat Transfer as `-amount`. Works
  iff Transfer is stored as one row per account, mirroring `signed_delta`'s
  current behavior on existing balances.

If you find both legs are stored as separate rows that already sum to zero
(via signed amounts in DB), use Option B. Otherwise use Option A and note
it as a limitation.

- [ ] **Step 5: No commit needed for this task** — findings inform later tasks.

---

## Task 1: Add `fx_rates` migration

**Files:**
- Create: `src/infrastructure/migrations/0006_fx_rates.sql`

- [ ] **Step 1: Write the migration**

```sql
CREATE TABLE fx_rates (
    rate_date     DATE    NOT NULL,
    from_currency TEXT    NOT NULL,
    to_currency   TEXT    NOT NULL,
    rate          NUMERIC NOT NULL,
    PRIMARY KEY (rate_date, from_currency, to_currency)
);

CREATE INDEX idx_fx_rates_date ON fx_rates (rate_date);
CREATE INDEX idx_fx_rates_from_currency ON fx_rates (from_currency, rate_date);
```

- [ ] **Step 2: Run the migration via tests to verify**

```bash
cargo test --no-run 2>&1 | tail -20
```

Expected: build succeeds (the migration runs in test setup automatically).

Then run a single existing test that hits the DB to verify migration applies:

```bash
cargo test --test api list_accounts_returns_empty_initially -- --nocapture 2>&1 | tail -20
```

Expected: PASS (or whatever the existing test name is — pick any existing passing integration test).

- [ ] **Step 3: Commit**

```bash
git add src/infrastructure/migrations/0006_fx_rates.sql
git commit -m "feat: add fx_rates table for currency conversion"
```

---

## Task 2: Add `user_settings` migration

**Files:**
- Create: `src/infrastructure/migrations/0007_user_settings.sql`

- [ ] **Step 1: Write the migration**

```sql
CREATE TABLE user_settings (
    user_id       UUID        PRIMARY KEY NOT NULL,
    base_currency TEXT        NOT NULL DEFAULT 'UAH',
    updated_at    TIMESTAMPTZ NOT NULL
);
```

- [ ] **Step 2: Verify it applies**

```bash
cargo test --no-run 2>&1 | tail -5
```

Expected: build succeeds.

- [ ] **Step 3: Commit**

```bash
git add src/infrastructure/migrations/0007_user_settings.sql
git commit -m "feat: add user_settings table for base_currency preference"
```

---

## Task 3: Domain — `FxRate` and traits

**Files:**
- Create: `src/domain/fx_rate.rs`
- Modify: `src/domain/mod.rs`

- [ ] **Step 1: Write the domain types and traits**

`src/domain/fx_rate.rs`:

```rust
use chrono::NaiveDate;
use rust_decimal::Decimal;

#[derive(Debug, Clone, PartialEq)]
pub struct FxRate {
    pub rate_date: NaiveDate,
    pub from_currency: String,
    pub to_currency: String,
    pub rate: Decimal,
}

#[async_trait::async_trait]
pub trait FxRateRepository: Send + Sync {
    /// Returns the rate for `from -> to` as of `date`, falling back to the
    /// most recent earlier rate. Returns `None` if no rate exists at all.
    /// Currencies are case-insensitive 3-letter codes.
    async fn rate_as_of(
        &self,
        date: NaiveDate,
        from: &str,
        to: &str,
    ) -> anyhow::Result<Option<Decimal>>;

    async fn upsert_many(&self, rates: &[FxRate]) -> anyhow::Result<()>;

    async fn latest_date(&self) -> anyhow::Result<Option<NaiveDate>>;

    /// Distinct `from_currency` values present in the table.
    async fn known_currencies(&self) -> anyhow::Result<Vec<String>>;
}

#[async_trait::async_trait]
pub trait FxRateSource: Send + Sync {
    /// Fetch all available rates against UAH for `date`.
    async fn fetch_rates_for(&self, date: NaiveDate) -> anyhow::Result<Vec<FxRate>>;
}
```

`src/domain/mod.rs`:

```rust
pub mod account;
pub mod bank_connection;
pub mod category;
pub mod error;
pub mod fx_rate;
pub mod monobank;
pub mod stats;
pub mod transaction;
pub mod user_settings;
```

(Order preserved alphabetical to match existing style; verify against the
current file and adjust additions to match.)

- [ ] **Step 2: Build**

```bash
cargo build 2>&1 | tail -20
```

Expected: compiles cleanly.

- [ ] **Step 3: Commit**

```bash
git add src/domain/fx_rate.rs src/domain/mod.rs
git commit -m "feat(domain): add FxRate value object and traits"
```

---

## Task 4: Domain — `UserSettings`

**Files:**
- Create: `src/domain/user_settings.rs`
- Modify: `src/domain/mod.rs` (already added in Task 3)

- [ ] **Step 1: Write the domain types**

`src/domain/user_settings.rs`:

```rust
use chrono::{DateTime, Utc};
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct UserSettings {
    pub user_id: Uuid,
    pub base_currency: String,
    pub updated_at: DateTime<Utc>,
}

impl UserSettings {
    pub fn default_for(user_id: Uuid) -> Self {
        Self {
            user_id,
            base_currency: "UAH".to_string(),
            updated_at: Utc::now(),
        }
    }
}

#[async_trait::async_trait]
pub trait UserSettingsRepository: Send + Sync {
    /// Returns `None` when the user has no row yet (defaults apply).
    async fn find(&self, user_id: Uuid) -> anyhow::Result<Option<UserSettings>>;

    async fn upsert(&self, settings: &UserSettings) -> anyhow::Result<()>;
}
```

- [ ] **Step 2: Build**

```bash
cargo build 2>&1 | tail -20
```

Expected: compiles cleanly.

- [ ] **Step 3: Commit**

```bash
git add src/domain/user_settings.rs src/domain/mod.rs
git commit -m "feat(domain): add UserSettings entity and repository trait"
```

---

## Task 5: Domain — Stats result types and `StatsRepository`

**Files:**
- Create: `src/domain/stats.rs`

- [ ] **Step 1: Write result types and trait**

`src/domain/stats.rs`:

```rust
use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::Decimal;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Granularity {
    Day,
    Month,
    Year,
}

impl Granularity {
    pub fn as_sql(&self) -> &'static str {
        match self {
            Granularity::Day => "day",
            Granularity::Month => "month",
            Granularity::Year => "year",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "day" => Some(Self::Day),
            "month" => Some(Self::Month),
            "year" => Some(Self::Year),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct MissingRate {
    pub date: NaiveDate,
    pub currency: String,
}

#[derive(Debug, Clone)]
pub struct StatsRange {
    pub user_id: Uuid,
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
    pub base_currency: String,
}

#[derive(Debug, Clone)]
pub struct DashboardStats {
    pub net_worth: Decimal,
    pub month_income: Decimal,
    pub month_expense: Decimal,
    pub top_categories: Vec<CategoryBreakdownItem>,
    pub missing_rates: Vec<MissingRate>,
}

#[derive(Debug, Clone)]
pub struct BalanceHistoryPoint {
    pub period_start: DateTime<Utc>,
    pub balance: Decimal,
}

#[derive(Debug, Clone)]
pub struct CashflowPoint {
    pub period_start: DateTime<Utc>,
    pub income: Decimal,
    pub expense: Decimal,
}

#[derive(Debug, Clone)]
pub struct CategoryBreakdownItem {
    pub category_id: Option<Uuid>,
    pub name: String,
    pub total: Decimal,
}

#[derive(Debug, Clone)]
pub struct TickerHolding {
    pub ticker: String,
    pub holdings: Decimal,
    pub cost_basis: Decimal,
    pub realized_pnl: Decimal,
    pub staking_received: Decimal,
}

/// One leg of trade history used by the service to compute average-cost
/// realized P&L. Returned in `transacted_at` order per ticker.
#[derive(Debug, Clone)]
pub struct TickerTradeLeg {
    pub ticker: String,
    pub kind: String, // "Buy" | "Sell" | "StakingReward"
    pub quantity: Decimal,
    pub amount_in_base: Decimal,
    pub transacted_at: DateTime<Utc>,
}

#[async_trait::async_trait]
pub trait StatsRepository: Send + Sync {
    async fn dashboard(
        &self,
        user_id: Uuid,
        base_currency: &str,
        top_n: i64,
    ) -> anyhow::Result<DashboardStats>;

    async fn balance_history(
        &self,
        range: &StatsRange,
        granularity: Granularity,
    ) -> anyhow::Result<(Vec<BalanceHistoryPoint>, Vec<MissingRate>)>;

    async fn cashflow(
        &self,
        range: &StatsRange,
        granularity: Granularity,
    ) -> anyhow::Result<(Vec<CashflowPoint>, Vec<MissingRate>)>;

    async fn categories(
        &self,
        range: &StatsRange,
        kind: &str,
    ) -> anyhow::Result<(Vec<CategoryBreakdownItem>, Vec<MissingRate>)>;

    /// Returns trade legs in `transacted_at` order per ticker, plus any
    /// missing-rate notices. Service layer computes average-cost P&L.
    async fn investment_trades(
        &self,
        user_id: Uuid,
        base_currency: &str,
    ) -> anyhow::Result<(Vec<TickerTradeLeg>, Vec<MissingRate>)>;
}
```

- [ ] **Step 2: Build**

```bash
cargo build 2>&1 | tail -20
```

Expected: compiles cleanly.

- [ ] **Step 3: Commit**

```bash
git add src/domain/stats.rs
git commit -m "feat(domain): add stats result types and StatsRepository trait"
```

---

## Task 6: NBU FX rate source — failing test

**Files:**
- Create: `src/infrastructure/nbu_client.rs`

NBU response shape (real API: `https://bank.gov.ua/NBUStatService/v1/statdirectory/exchange?date=YYYYMMDD&json`):

```json
[
  {"r030":840,"txt":"Долар США","rate":40.5,"cc":"USD","exchangedate":"10.05.2026"},
  {"r030":978,"txt":"Євро","rate":43.2,"cc":"EUR","exchangedate":"10.05.2026"}
]
```

We need `NbuFxRateSource` to convert this into `Vec<FxRate>` with
`to_currency = "UAH"`.

- [ ] **Step 1: Write the failing unit test for response parsing**

In `src/infrastructure/nbu_client.rs` add a `#[cfg(test)] mod tests` block:

```rust
use async_trait::async_trait;
use chrono::NaiveDate;
use rust_decimal::Decimal;
use serde::Deserialize;

use crate::domain::fx_rate::{FxRate, FxRateSource};

const NBU_URL: &str = "https://bank.gov.ua/NBUStatService/v1/statdirectory/exchange";

pub struct NbuFxRateSource {
    http: reqwest::Client,
    base_url: String,
}

impl Default for NbuFxRateSource {
    fn default() -> Self {
        Self::new()
    }
}

impl NbuFxRateSource {
    pub fn new() -> Self {
        Self {
            http: reqwest::Client::new(),
            base_url: NBU_URL.to_string(),
        }
    }

    #[cfg(test)]
    pub fn with_base_url(base_url: String) -> Self {
        Self {
            http: reqwest::Client::new(),
            base_url,
        }
    }
}

#[derive(Debug, Deserialize)]
struct NbuRow {
    cc: String,
    rate: Decimal,
}

fn parse_rows(date: NaiveDate, rows: Vec<NbuRow>) -> Vec<FxRate> {
    rows.into_iter()
        .map(|row| FxRate {
            rate_date: date,
            from_currency: row.cc,
            to_currency: "UAH".to_string(),
            rate: row.rate,
        })
        .collect()
}

#[async_trait]
impl FxRateSource for NbuFxRateSource {
    async fn fetch_rates_for(&self, date: NaiveDate) -> anyhow::Result<Vec<FxRate>> {
        let date_param = date.format("%Y%m%d").to_string();
        let url = format!("{}?date={}&json", self.base_url, date_param);
        let rows: Vec<NbuRow> = self.http.get(url).send().await?.json().await?;
        Ok(parse_rows(date, rows))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_nbu_rows_to_fx_rates() {
        let date = NaiveDate::from_ymd_opt(2026, 5, 10).unwrap();
        let rows = vec![
            NbuRow { cc: "USD".to_string(), rate: Decimal::new(405, 1) },
            NbuRow { cc: "EUR".to_string(), rate: Decimal::new(432, 1) },
        ];

        let rates = parse_rows(date, rows);

        assert_eq!(rates.len(), 2);
        assert_eq!(rates[0].from_currency, "USD");
        assert_eq!(rates[0].to_currency, "UAH");
        assert_eq!(rates[0].rate_date, date);
        assert_eq!(rates[0].rate, Decimal::new(405, 1));
    }
}
```

Also register the module in `src/infrastructure/mod.rs`:

```rust
pub mod account_repository;
pub mod category_repository;
pub mod db;
pub mod monobank_client;
pub mod monobank_repository;
pub mod nbu_client;
#[cfg(test)]
pub mod test_db;
pub mod transaction_repository;
```

(Adjust to match existing entries; just add `pub mod nbu_client;` in the
right place.)

- [ ] **Step 2: Run the test**

```bash
cargo test --lib infrastructure::nbu_client::tests -- --nocapture 2>&1 | tail -20
```

Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add src/infrastructure/nbu_client.rs src/infrastructure/mod.rs
git commit -m "feat(infra): add NBU FX rate source"
```

---

## Task 7: `PgFxRateRepository` — schema CRUD + rate-as-of lookup

**Files:**
- Create: `src/infrastructure/fx_rate_repository.rs`
- Modify: `src/infrastructure/mod.rs` (add `pub mod fx_rate_repository;`)

- [ ] **Step 1: Write the implementation**

`src/infrastructure/fx_rate_repository.rs`:

```rust
use async_trait::async_trait;
use chrono::NaiveDate;
use rust_decimal::Decimal;
use sqlx::PgPool;

use crate::domain::fx_rate::{FxRate, FxRateRepository};

pub struct PgFxRateRepository {
    pool: PgPool,
}

impl PgFxRateRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl FxRateRepository for PgFxRateRepository {
    async fn rate_as_of(
        &self,
        date: NaiveDate,
        from: &str,
        to: &str,
    ) -> anyhow::Result<Option<Decimal>> {
        let from = from.to_uppercase();
        let to = to.to_uppercase();
        if from == to {
            return Ok(Some(Decimal::ONE));
        }
        // Canonical storage is `*->UAH`; derive cross rates as needed.
        let from_to_uah = if from == "UAH" {
            Some(Decimal::ONE)
        } else {
            sqlx::query_scalar!(
                r#"
                SELECT rate FROM fx_rates
                WHERE from_currency = $1 AND to_currency = 'UAH' AND rate_date <= $2
                ORDER BY rate_date DESC
                LIMIT 1
                "#,
                from,
                date,
            )
            .fetch_optional(&self.pool)
            .await?
        };
        let to_to_uah = if to == "UAH" {
            Some(Decimal::ONE)
        } else {
            sqlx::query_scalar!(
                r#"
                SELECT rate FROM fx_rates
                WHERE from_currency = $1 AND to_currency = 'UAH' AND rate_date <= $2
                ORDER BY rate_date DESC
                LIMIT 1
                "#,
                to,
                date,
            )
            .fetch_optional(&self.pool)
            .await?
        };
        match (from_to_uah, to_to_uah) {
            (Some(f), Some(t)) if !t.is_zero() => Ok(Some(f / t)),
            _ => Ok(None),
        }
    }

    async fn upsert_many(&self, rates: &[FxRate]) -> anyhow::Result<()> {
        if rates.is_empty() {
            return Ok(());
        }
        let mut tx = self.pool.begin().await?;
        for r in rates {
            sqlx::query!(
                r#"
                INSERT INTO fx_rates (rate_date, from_currency, to_currency, rate)
                VALUES ($1, $2, $3, $4)
                ON CONFLICT (rate_date, from_currency, to_currency)
                DO UPDATE SET rate = EXCLUDED.rate
                "#,
                r.rate_date,
                r.from_currency.to_uppercase(),
                r.to_currency.to_uppercase(),
                r.rate,
            )
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    async fn latest_date(&self) -> anyhow::Result<Option<NaiveDate>> {
        Ok(sqlx::query_scalar!("SELECT MAX(rate_date) FROM fx_rates")
            .fetch_one(&self.pool)
            .await?)
    }

    async fn known_currencies(&self) -> anyhow::Result<Vec<String>> {
        let rows = sqlx::query_scalar!(
            "SELECT DISTINCT from_currency FROM fx_rates ORDER BY from_currency"
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }
}
```

- [ ] **Step 2: Add `pub mod fx_rate_repository;` to `src/infrastructure/mod.rs`**

- [ ] **Step 3: Prepare sqlx offline metadata**

```bash
cargo sqlx prepare -- --tests 2>&1 | tail -10
```

If `cargo sqlx` is not installed, install with:

```bash
cargo install sqlx-cli --no-default-features --features postgres
```

Expected: `.sqlx/` is updated. (Requires `DATABASE_URL` set or use a live DB; alternatively run `SQLX_OFFLINE=false cargo build` against a real Postgres.)

- [ ] **Step 4: Build**

```bash
cargo build 2>&1 | tail -20
```

Expected: compiles cleanly.

- [ ] **Step 5: Commit**

```bash
git add src/infrastructure/fx_rate_repository.rs src/infrastructure/mod.rs .sqlx/
git commit -m "feat(infra): add PgFxRateRepository with rate-as-of lookup"
```

---

## Task 8: Integration test — FX repo

**Files:**
- Create: `tests/api/fx_rates.rs`
- Modify: `tests/api/mod.rs` (add `pub mod fx_rates;`)
- Modify: `tests/api/helpers.rs` (add `seed_fx_rate` helper)

- [ ] **Step 1: Add helper for seeding rates**

In `tests/api/helpers.rs`, append:

```rust
use chrono::NaiveDate;
use rust_decimal::Decimal;
use sqlx::PgPool;

pub async fn seed_fx_rate(
    pool: &PgPool,
    date: NaiveDate,
    from: &str,
    rate: Decimal,
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
```

(If `chrono`/`rust_decimal`/`sqlx` aren't already imported at the top of
the file, add them.)

- [ ] **Step 2: Write integration tests**

`tests/api/fx_rates.rs`:

```rust
use chrono::NaiveDate;
use moneykeeper::domain::fx_rate::{FxRate, FxRateRepository};
use moneykeeper::infrastructure::fx_rate_repository::PgFxRateRepository;
use rust_decimal::Decimal;

use crate::common::TestPostgres;
use crate::api::helpers::seed_fx_rate;

#[tokio::test]
async fn rate_as_of_returns_exact_date_rate() {
    let db = TestPostgres::new().await;
    let date = NaiveDate::from_ymd_opt(2026, 5, 10).unwrap();
    seed_fx_rate(&db.pool, date, "USD", Decimal::new(405, 1)).await;

    let repo = PgFxRateRepository::new(db.pool.clone());
    let rate = repo.rate_as_of(date, "USD", "UAH").await.unwrap();

    assert_eq!(rate, Some(Decimal::new(405, 1)));
}

#[tokio::test]
async fn rate_as_of_falls_back_to_earlier_date() {
    let db = TestPostgres::new().await;
    let earlier = NaiveDate::from_ymd_opt(2026, 5, 1).unwrap();
    let query_date = NaiveDate::from_ymd_opt(2026, 5, 10).unwrap();
    seed_fx_rate(&db.pool, earlier, "USD", Decimal::new(400, 1)).await;

    let repo = PgFxRateRepository::new(db.pool.clone());
    let rate = repo.rate_as_of(query_date, "USD", "UAH").await.unwrap();

    assert_eq!(rate, Some(Decimal::new(400, 1)));
}

#[tokio::test]
async fn rate_as_of_returns_none_when_no_rate_exists() {
    let db = TestPostgres::new().await;
    let repo = PgFxRateRepository::new(db.pool.clone());

    let rate = repo
        .rate_as_of(NaiveDate::from_ymd_opt(2026, 5, 10).unwrap(), "USD", "UAH")
        .await
        .unwrap();

    assert_eq!(rate, None);
}

#[tokio::test]
async fn rate_as_of_identity_for_same_currency() {
    let db = TestPostgres::new().await;
    let repo = PgFxRateRepository::new(db.pool.clone());

    let rate = repo
        .rate_as_of(NaiveDate::from_ymd_opt(2026, 5, 10).unwrap(), "UAH", "UAH")
        .await
        .unwrap();

    assert_eq!(rate, Some(Decimal::ONE));
}

#[tokio::test]
async fn rate_as_of_cross_rate_via_uah() {
    let db = TestPostgres::new().await;
    let date = NaiveDate::from_ymd_opt(2026, 5, 10).unwrap();
    // 1 USD = 40 UAH, 1 EUR = 50 UAH → 1 USD = 0.8 EUR
    seed_fx_rate(&db.pool, date, "USD", Decimal::new(40, 0)).await;
    seed_fx_rate(&db.pool, date, "EUR", Decimal::new(50, 0)).await;

    let repo = PgFxRateRepository::new(db.pool.clone());
    let rate = repo.rate_as_of(date, "USD", "EUR").await.unwrap();

    assert_eq!(rate, Some(Decimal::new(8, 1))); // 0.8
}

#[tokio::test]
async fn upsert_many_replaces_existing() {
    let db = TestPostgres::new().await;
    let date = NaiveDate::from_ymd_opt(2026, 5, 10).unwrap();
    seed_fx_rate(&db.pool, date, "USD", Decimal::new(40, 0)).await;

    let repo = PgFxRateRepository::new(db.pool.clone());
    repo.upsert_many(&[FxRate {
        rate_date: date,
        from_currency: "USD".to_string(),
        to_currency: "UAH".to_string(),
        rate: Decimal::new(45, 0),
    }])
    .await
    .unwrap();

    let rate = repo.rate_as_of(date, "USD", "UAH").await.unwrap();
    assert_eq!(rate, Some(Decimal::new(45, 0)));
}
```

- [ ] **Step 3: Register the module in `tests/api/mod.rs`**

Add `pub mod fx_rates;` (location depending on existing structure).

- [ ] **Step 4: Run tests**

```bash
cargo test --test api fx_rates -- --nocapture 2>&1 | tail -30
```

Expected: all 6 tests pass.

- [ ] **Step 5: Commit**

```bash
git add tests/api/fx_rates.rs tests/api/helpers.rs tests/api/mod.rs
git commit -m "test(infra): integration tests for PgFxRateRepository"
```

---

## Task 9: `FxSyncUseCase`

**Files:**
- Create: `src/application/fx_sync.rs`
- Modify: `src/application/mod.rs`

- [ ] **Step 1: Write the use case with unit tests**

`src/application/fx_sync.rs`:

```rust
use std::sync::Arc;

use chrono::{Duration, NaiveDate, Utc};

use crate::domain::fx_rate::{FxRateRepository, FxRateSource};

pub struct FxSyncUseCase {
    source: Arc<dyn FxRateSource>,
    repo: Arc<dyn FxRateRepository>,
}

impl FxSyncUseCase {
    pub fn new(source: Arc<dyn FxRateSource>, repo: Arc<dyn FxRateRepository>) -> Self {
        Self { source, repo }
    }

    pub async fn sync_today(&self) -> anyhow::Result<usize> {
        self.sync_date(Utc::now().date_naive()).await
    }

    pub async fn sync_date(&self, date: NaiveDate) -> anyhow::Result<usize> {
        let rates = self.source.fetch_rates_for(date).await?;
        let n = rates.len();
        self.repo.upsert_many(&rates).await?;
        Ok(n)
    }

    /// Fetches and upserts rates for every date in `[from, to]`.
    pub async fn backfill(&self, from: NaiveDate, to: NaiveDate) -> anyhow::Result<usize> {
        let mut total = 0;
        let mut d = from;
        while d <= to {
            total += self.sync_date(d).await?;
            d += Duration::days(1);
        }
        Ok(total)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::fx_rate::{FxRate, FxRateSource};
    use async_trait::async_trait;
    use rust_decimal::Decimal;
    use std::sync::Mutex;

    #[derive(Default)]
    struct FakeSource {
        calls: Mutex<Vec<NaiveDate>>,
    }

    #[async_trait]
    impl FxRateSource for FakeSource {
        async fn fetch_rates_for(&self, date: NaiveDate) -> anyhow::Result<Vec<FxRate>> {
            self.calls.lock().unwrap().push(date);
            Ok(vec![FxRate {
                rate_date: date,
                from_currency: "USD".to_string(),
                to_currency: "UAH".to_string(),
                rate: Decimal::new(40, 0),
            }])
        }
    }

    #[derive(Default)]
    struct FakeRepo {
        upserts: Mutex<Vec<FxRate>>,
    }

    #[async_trait]
    impl FxRateRepository for FakeRepo {
        async fn rate_as_of(
            &self,
            _date: NaiveDate,
            _from: &str,
            _to: &str,
        ) -> anyhow::Result<Option<Decimal>> {
            unimplemented!()
        }
        async fn upsert_many(&self, rates: &[FxRate]) -> anyhow::Result<()> {
            self.upserts.lock().unwrap().extend(rates.iter().cloned());
            Ok(())
        }
        async fn latest_date(&self) -> anyhow::Result<Option<NaiveDate>> {
            unimplemented!()
        }
        async fn known_currencies(&self) -> anyhow::Result<Vec<String>> {
            unimplemented!()
        }
    }

    #[tokio::test]
    async fn sync_date_fetches_and_upserts() {
        let source = Arc::new(FakeSource::default());
        let repo = Arc::new(FakeRepo::default());
        let usecase = FxSyncUseCase::new(source.clone(), repo.clone());

        let date = NaiveDate::from_ymd_opt(2026, 5, 10).unwrap();
        let n = usecase.sync_date(date).await.unwrap();

        assert_eq!(n, 1);
        assert_eq!(*source.calls.lock().unwrap(), vec![date]);
        assert_eq!(repo.upserts.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn backfill_calls_source_for_each_date_inclusive() {
        let source = Arc::new(FakeSource::default());
        let repo = Arc::new(FakeRepo::default());
        let usecase = FxSyncUseCase::new(source.clone(), repo.clone());

        let from = NaiveDate::from_ymd_opt(2026, 5, 8).unwrap();
        let to = NaiveDate::from_ymd_opt(2026, 5, 10).unwrap();
        usecase.backfill(from, to).await.unwrap();

        assert_eq!(source.calls.lock().unwrap().len(), 3);
    }
}
```

- [ ] **Step 2: Add `pub mod fx_sync;` to `src/application/mod.rs`**

- [ ] **Step 3: Run tests**

```bash
cargo test --lib application::fx_sync 2>&1 | tail -20
```

Expected: 2 tests pass.

- [ ] **Step 4: Commit**

```bash
git add src/application/fx_sync.rs src/application/mod.rs
git commit -m "feat(application): add FxSyncUseCase with backfill"
```

---

## Task 10: `PgUserSettingsRepository`

**Files:**
- Create: `src/infrastructure/user_settings_repository.rs`
- Modify: `src/infrastructure/mod.rs`

- [ ] **Step 1: Write implementation**

```rust
use async_trait::async_trait;
use chrono::Utc;
use sqlx::PgPool;
use uuid::Uuid;

use crate::domain::user_settings::{UserSettings, UserSettingsRepository};

pub struct PgUserSettingsRepository {
    pool: PgPool,
}

impl PgUserSettingsRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl UserSettingsRepository for PgUserSettingsRepository {
    async fn find(&self, user_id: Uuid) -> anyhow::Result<Option<UserSettings>> {
        let row = sqlx::query!(
            "SELECT user_id, base_currency, updated_at
             FROM user_settings
             WHERE user_id = $1",
            user_id,
        )
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(|r| UserSettings {
            user_id: r.user_id,
            base_currency: r.base_currency,
            updated_at: r.updated_at,
        }))
    }

    async fn upsert(&self, settings: &UserSettings) -> anyhow::Result<()> {
        sqlx::query!(
            r#"
            INSERT INTO user_settings (user_id, base_currency, updated_at)
            VALUES ($1, $2, $3)
            ON CONFLICT (user_id) DO UPDATE
            SET base_currency = EXCLUDED.base_currency,
                updated_at    = EXCLUDED.updated_at
            "#,
            settings.user_id,
            settings.base_currency.to_uppercase(),
            Utc::now(),
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}
```

- [ ] **Step 2: Register in mod.rs**

Add `pub mod user_settings_repository;` to `src/infrastructure/mod.rs`.

- [ ] **Step 3: Prepare sqlx offline metadata**

```bash
cargo sqlx prepare -- --tests 2>&1 | tail -10
```

- [ ] **Step 4: Build**

```bash
cargo build 2>&1 | tail -20
```

Expected: compiles cleanly.

- [ ] **Step 5: Commit**

```bash
git add src/infrastructure/user_settings_repository.rs src/infrastructure/mod.rs .sqlx/
git commit -m "feat(infra): add PgUserSettingsRepository"
```

---

## Task 11: `UserSettingsService` + handler

**Files:**
- Create: `src/application/user_settings.rs`
- Create: `src/api/handlers/user_settings.rs`
- Modify: `src/application/mod.rs`, `src/api/handlers/mod.rs`, `src/api/dto.rs`, `src/api/state.rs`, `src/api/routes.rs`, `src/main.rs`

- [ ] **Step 1: Service**

`src/application/user_settings.rs`:

```rust
use std::sync::Arc;

use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::fx_rate::FxRateRepository;
use crate::domain::user_settings::{UserSettings, UserSettingsRepository};

pub struct UserSettingsService {
    repo: Arc<dyn UserSettingsRepository>,
    fx_repo: Arc<dyn FxRateRepository>,
}

impl UserSettingsService {
    pub fn new(
        repo: Arc<dyn UserSettingsRepository>,
        fx_repo: Arc<dyn FxRateRepository>,
    ) -> Self {
        Self { repo, fx_repo }
    }

    pub async fn get_or_default(&self, user_id: Uuid) -> anyhow::Result<UserSettings> {
        match self.repo.find(user_id).await? {
            Some(s) => Ok(s),
            None => Ok(UserSettings::default_for(user_id)),
        }
    }

    pub async fn set_base_currency(
        &self,
        user_id: Uuid,
        base_currency: &str,
    ) -> anyhow::Result<UserSettings> {
        let normalized = base_currency.to_uppercase();
        if normalized.len() != 3 {
            return Err(DomainError::InvalidInput(
                "base_currency must be a 3-letter ISO code".to_string(),
            )
            .into());
        }
        if normalized != "UAH" {
            let known = self.fx_repo.known_currencies().await?;
            if !known.iter().any(|c| c == &normalized) {
                return Err(DomainError::InvalidInput(format!(
                    "unknown currency: {normalized}"
                ))
                .into());
            }
        }
        let mut s = self.get_or_default(user_id).await?;
        s.base_currency = normalized;
        self.repo.upsert(&s).await?;
        Ok(s)
    }
}
```

Add `pub mod user_settings;` to `src/application/mod.rs`.

- [ ] **Step 2: DTOs**

In `src/api/dto.rs` append:

```rust
#[derive(Serialize)]
pub struct UserSettingsResponse {
    pub base_currency: String,
}

#[derive(Deserialize)]
pub struct UpdateUserSettingsRequest {
    pub base_currency: String,
}
```

- [ ] **Step 3: Handler**

`src/api/handlers/user_settings.rs`:

```rust
use axum::Json;
use axum::extract::{Extension, State};

use crate::api::dto::{UpdateUserSettingsRequest, UserSettingsResponse};
use crate::api::error::AppError;
use crate::api::middleware::AuthUser;
use crate::api::state::AppState;

pub async fn get_settings(
    State(state): State<AppState>,
    Extension(AuthUser(user_id)): Extension<AuthUser>,
) -> Result<Json<UserSettingsResponse>, AppError> {
    let s = state.user_settings.get_or_default(user_id).await?;
    Ok(Json(UserSettingsResponse {
        base_currency: s.base_currency,
    }))
}

pub async fn update_settings(
    State(state): State<AppState>,
    Extension(AuthUser(user_id)): Extension<AuthUser>,
    Json(req): Json<UpdateUserSettingsRequest>,
) -> Result<Json<UserSettingsResponse>, AppError> {
    let s = state
        .user_settings
        .set_base_currency(user_id, &req.base_currency)
        .await?;
    Ok(Json(UserSettingsResponse {
        base_currency: s.base_currency,
    }))
}
```

Add `pub mod user_settings;` to `src/api/handlers/mod.rs`.

- [ ] **Step 4: AppState + routes + main wiring**

In `src/api/state.rs`:

```rust
use crate::application::user_settings::UserSettingsService;
// inside AppState:
pub user_settings: Arc<UserSettingsService>,
```

In `src/api/routes.rs`, inside the protected router:

```rust
.route(
    "/me/settings",
    axum::routing::get(crate::api::handlers::user_settings::get_settings)
        .patch(crate::api::handlers::user_settings::update_settings),
)
```

In `src/main.rs`, after creating `account_repo`/`transaction_repo`:

```rust
let fx_repo: Arc<dyn moneykeeper::domain::fx_rate::FxRateRepository> = Arc::new(
    moneykeeper::infrastructure::fx_rate_repository::PgFxRateRepository::new(pool.clone()),
);
let user_settings_repo: Arc<dyn moneykeeper::domain::user_settings::UserSettingsRepository> =
    Arc::new(
        moneykeeper::infrastructure::user_settings_repository::PgUserSettingsRepository::new(
            pool.clone(),
        ),
    );
```

And in the `AppState { ... }` literal:

```rust
user_settings: Arc::new(
    moneykeeper::application::user_settings::UserSettingsService::new(
        Arc::clone(&user_settings_repo),
        Arc::clone(&fx_repo),
    ),
),
```

- [ ] **Step 5: Build**

```bash
cargo build 2>&1 | tail -20
```

Expected: compiles cleanly.

- [ ] **Step 6: Integration test**

In `tests/api/stats.rs` (creating it now if not present):

```rust
use crate::api::helpers::{auth_token, build_app, seed_fx_rate};
use crate::common::TestPostgres;
use chrono::NaiveDate;
use rust_decimal::Decimal;
use serde_json::json;

#[tokio::test]
async fn user_settings_default_is_uah() {
    let db = TestPostgres::new().await;
    let app = build_app(db.pool.clone()).await;
    let (token, _) = auth_token();

    let response = app
        .get("/me/settings")
        .add_header("authorization", format!("Bearer {token}"))
        .await;

    response.assert_status_ok();
    response.assert_json(&json!({ "base_currency": "UAH" }));
}

#[tokio::test]
async fn user_settings_patch_persists() {
    let db = TestPostgres::new().await;
    seed_fx_rate(
        &db.pool,
        NaiveDate::from_ymd_opt(2026, 5, 10).unwrap(),
        "USD",
        Decimal::new(40, 0),
    )
    .await;
    let app = build_app(db.pool.clone()).await;
    let (token, _) = auth_token();

    let response = app
        .patch("/me/settings")
        .add_header("authorization", format!("Bearer {token}"))
        .json(&json!({ "base_currency": "USD" }))
        .await;

    response.assert_status_ok();
    response.assert_json(&json!({ "base_currency": "USD" }));

    let get_response = app
        .get("/me/settings")
        .add_header("authorization", format!("Bearer {token}"))
        .await;
    get_response.assert_json(&json!({ "base_currency": "USD" }));
}

#[tokio::test]
async fn user_settings_patch_rejects_unknown_currency() {
    let db = TestPostgres::new().await;
    let app = build_app(db.pool.clone()).await;
    let (token, _) = auth_token();

    let response = app
        .patch("/me/settings")
        .add_header("authorization", format!("Bearer {token}"))
        .json(&json!({ "base_currency": "ZZZ" }))
        .await;

    response.assert_status(http::StatusCode::BAD_REQUEST);
}
```

Notes:
- `build_app` and `auth_token` are existing helpers — verify their actual
  signatures by reading `tests/api/helpers.rs` and adapt the calls. If the
  app builder doesn't yet take a pool, follow whatever pattern the existing
  account tests use.
- Add `pub mod stats;` to `tests/api/mod.rs`.

- [ ] **Step 7: Run the tests**

```bash
cargo test --test api stats::user_settings 2>&1 | tail -30
```

Expected: 3 tests pass.

- [ ] **Step 8: Commit**

```bash
git add src/application/user_settings.rs src/application/mod.rs \
       src/api/handlers/user_settings.rs src/api/handlers/mod.rs \
       src/api/dto.rs src/api/state.rs src/api/routes.rs src/main.rs \
       tests/api/stats.rs tests/api/mod.rs
git commit -m "feat: GET/PATCH /me/settings with base_currency validation"
```

---

## Task 12: `PgStatsRepository` skeleton + categories endpoint

**Files:**
- Create: `src/infrastructure/stats_repository.rs`
- Modify: `src/infrastructure/mod.rs`

- [ ] **Step 1: Skeleton**

```rust
use async_trait::async_trait;
use chrono::NaiveDate;
use rust_decimal::Decimal;
use sqlx::PgPool;
use uuid::Uuid;

use crate::domain::stats::{
    BalanceHistoryPoint, CashflowPoint, CategoryBreakdownItem, DashboardStats, Granularity,
    MissingRate, StatsRange, StatsRepository, TickerTradeLeg,
};

pub struct PgStatsRepository {
    pool: PgPool,
}

impl PgStatsRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

// Transfer convention (verified during Task 0): <fill in once known>
//
// Aggregation rule for balance/cashflow uses Income, Expense, Buy, Sell,
// StakingReward kinds. Transfers are <included|excluded> per the rule
// chosen in Task 0.

#[async_trait]
impl StatsRepository for PgStatsRepository {
    async fn dashboard(
        &self,
        _user_id: Uuid,
        _base_currency: &str,
        _top_n: i64,
    ) -> anyhow::Result<DashboardStats> {
        unimplemented!("Task 16")
    }

    async fn balance_history(
        &self,
        _range: &StatsRange,
        _granularity: Granularity,
    ) -> anyhow::Result<(Vec<BalanceHistoryPoint>, Vec<MissingRate>)> {
        unimplemented!("Task 14")
    }

    async fn cashflow(
        &self,
        _range: &StatsRange,
        _granularity: Granularity,
    ) -> anyhow::Result<(Vec<CashflowPoint>, Vec<MissingRate>)> {
        unimplemented!("Task 13")
    }

    async fn categories(
        &self,
        range: &StatsRange,
        kind: &str,
    ) -> anyhow::Result<(Vec<CategoryBreakdownItem>, Vec<MissingRate>)> {
        let from_date: NaiveDate = range.from.date_naive();
        let to_date: NaiveDate = range.to.date_naive();
        let base = range.base_currency.to_uppercase();

        // rate_cte: for each (date, currency) used in the range, pick the
        // most-recent <= that date `*->UAH` rate, then express as a rate to
        // base_currency by dividing by base->UAH on the same date.
        let rows = sqlx::query!(
            r#"
            WITH range_txs AS (
                SELECT t.id, t.amount, t.currency, t.category_id,
                       t.transacted_at::date AS tx_date
                FROM transactions t
                WHERE t.user_id = $1
                  AND t.kind = $2
                  AND t.transacted_at >= $3
                  AND t.transacted_at <  $4
            ),
            rates AS (
                SELECT rt.tx_date, rt.currency,
                    (SELECT rate FROM fx_rates
                     WHERE from_currency = rt.currency AND to_currency = 'UAH'
                       AND rate_date <= rt.tx_date
                     ORDER BY rate_date DESC LIMIT 1) AS to_uah,
                    (SELECT rate FROM fx_rates
                     WHERE from_currency = $5 AND to_currency = 'UAH'
                       AND rate_date <= rt.tx_date
                     ORDER BY rate_date DESC LIMIT 1) AS base_to_uah
                FROM (SELECT DISTINCT tx_date, currency FROM range_txs) rt
            ),
            converted AS (
                SELECT rt.id, rt.category_id,
                    CASE
                        WHEN rt.currency = $5 THEN rt.amount
                        WHEN rt.currency = 'UAH' AND $5 = 'UAH' THEN rt.amount
                        WHEN rt.currency = 'UAH' AND r.base_to_uah IS NOT NULL
                            THEN rt.amount / r.base_to_uah
                        WHEN $5 = 'UAH' AND r.to_uah IS NOT NULL
                            THEN rt.amount * r.to_uah
                        WHEN r.to_uah IS NOT NULL AND r.base_to_uah IS NOT NULL
                            THEN rt.amount * r.to_uah / r.base_to_uah
                        ELSE NULL
                    END AS amount_in_base,
                    rt.tx_date, rt.currency
                FROM range_txs rt
                LEFT JOIN rates r ON r.tx_date = rt.tx_date AND r.currency = rt.currency
            ),
            missing AS (
                SELECT DISTINCT tx_date, currency
                FROM converted
                WHERE amount_in_base IS NULL
                LIMIT 10
            ),
            totals AS (
                SELECT category_id, SUM(amount_in_base) AS total
                FROM converted
                WHERE amount_in_base IS NOT NULL
                GROUP BY category_id
            )
            SELECT t.category_id,
                   c.name,
                   t.total,
                   (SELECT json_agg(row_to_json(missing.*)) FROM missing) AS missing_json
            FROM totals t
            LEFT JOIN categories c ON c.id = t.category_id
            ORDER BY t.total DESC NULLS LAST
            "#,
            range.user_id,
            kind,
            range.from,
            range.to,
            base,
        )
        .fetch_all(&self.pool)
        .await?;

        let mut items: Vec<CategoryBreakdownItem> = Vec::new();
        let mut missing: Vec<MissingRate> = Vec::new();
        for (i, r) in rows.iter().enumerate() {
            items.push(CategoryBreakdownItem {
                category_id: r.category_id,
                name: r.name.clone().unwrap_or_else(|| "Uncategorized".to_string()),
                total: r.total.unwrap_or(Decimal::ZERO),
            });
            if i == 0 {
                if let Some(json) = &r.missing_json {
                    let parsed: Vec<serde_json::Value> =
                        serde_json::from_value(json.clone()).unwrap_or_default();
                    for v in parsed {
                        if let (Some(d), Some(c)) = (
                            v.get("tx_date").and_then(|x| x.as_str()),
                            v.get("currency").and_then(|x| x.as_str()),
                        ) {
                            if let Ok(parsed_d) = NaiveDate::parse_from_str(d, "%Y-%m-%d") {
                                missing.push(MissingRate {
                                    date: parsed_d,
                                    currency: c.to_string(),
                                });
                            }
                        }
                    }
                }
            }
        }
        Ok((items, missing))
    }

    async fn investment_trades(
        &self,
        _user_id: Uuid,
        _base_currency: &str,
    ) -> anyhow::Result<(Vec<TickerTradeLeg>, Vec<MissingRate>)> {
        unimplemented!("Task 15")
    }
}
```

Note: the SQL is intentionally explicit rather than using the cleaner CTE
pattern from the spec — the explicit form is easier to debug. It can be
factored later once all 5 endpoints exist and the shape stabilizes.

Add `pub mod stats_repository;` to `src/infrastructure/mod.rs`.

- [ ] **Step 2: Prepare sqlx and build**

```bash
cargo sqlx prepare -- --tests 2>&1 | tail -10
cargo build 2>&1 | tail -20
```

Expected: compiles cleanly.

- [ ] **Step 3: Commit**

```bash
git add src/infrastructure/stats_repository.rs src/infrastructure/mod.rs .sqlx/
git commit -m "feat(infra): PgStatsRepository skeleton + categories aggregation"
```

---

## Task 13: Cashflow aggregation

**Files:**
- Modify: `src/infrastructure/stats_repository.rs`

- [ ] **Step 1: Implement `cashflow`**

Replace the `unimplemented!()` body of `cashflow` with:

```rust
let base = range.base_currency.to_uppercase();
let gran = granularity.as_sql();
let bucket_expr = format!("date_trunc('{}', t.transacted_at) AT TIME ZONE 'UTC'", gran);

// We can't safely interpolate `gran` into a query macro — use query_as
// with text substitution since `gran` comes from a small enum.
let sql = format!(
    r#"
    WITH range_txs AS (
        SELECT t.id, t.amount, t.currency, t.kind,
               t.transacted_at,
               t.transacted_at::date AS tx_date,
               {bucket_expr} AS bucket
        FROM transactions t
        WHERE t.user_id = $1
          AND t.kind IN ('Income', 'Expense')
          AND t.transacted_at >= $2
          AND t.transacted_at <  $3
    ),
    rates AS (
        SELECT rt.tx_date, rt.currency,
            (SELECT rate FROM fx_rates
             WHERE from_currency = rt.currency AND to_currency = 'UAH'
               AND rate_date <= rt.tx_date
             ORDER BY rate_date DESC LIMIT 1) AS to_uah,
            (SELECT rate FROM fx_rates
             WHERE from_currency = $4 AND to_currency = 'UAH'
               AND rate_date <= rt.tx_date
             ORDER BY rate_date DESC LIMIT 1) AS base_to_uah
        FROM (SELECT DISTINCT tx_date, currency FROM range_txs) rt
    ),
    converted AS (
        SELECT rt.bucket, rt.kind,
            CASE
                WHEN rt.currency = $4 THEN rt.amount
                WHEN rt.currency = 'UAH' AND $4 = 'UAH' THEN rt.amount
                WHEN rt.currency = 'UAH' AND r.base_to_uah IS NOT NULL
                    THEN rt.amount / r.base_to_uah
                WHEN $4 = 'UAH' AND r.to_uah IS NOT NULL
                    THEN rt.amount * r.to_uah
                WHEN r.to_uah IS NOT NULL AND r.base_to_uah IS NOT NULL
                    THEN rt.amount * r.to_uah / r.base_to_uah
                ELSE NULL
            END AS amount_in_base,
            rt.tx_date, rt.currency
        FROM range_txs rt
        LEFT JOIN rates r ON r.tx_date = rt.tx_date AND r.currency = rt.currency
    )
    SELECT
        bucket AS "bucket!",
        SUM(CASE WHEN kind = 'Income'  THEN amount_in_base ELSE 0 END) AS income,
        SUM(CASE WHEN kind = 'Expense' THEN amount_in_base ELSE 0 END) AS expense
    FROM converted
    WHERE amount_in_base IS NOT NULL
    GROUP BY bucket
    ORDER BY bucket
    "#
);

#[derive(sqlx::FromRow)]
struct Row {
    bucket: chrono::DateTime<chrono::Utc>,
    income: Option<Decimal>,
    expense: Option<Decimal>,
}

let rows: Vec<Row> = sqlx::query_as(&sql)
    .bind(range.user_id)
    .bind(range.from)
    .bind(range.to)
    .bind(&base)
    .fetch_all(&self.pool)
    .await?;

let points = rows
    .into_iter()
    .map(|r| CashflowPoint {
        period_start: r.bucket,
        income: r.income.unwrap_or(Decimal::ZERO),
        expense: r.expense.unwrap_or(Decimal::ZERO),
    })
    .collect();

// Missing rates query (separate, since the CTE above filters them out)
let missing_rows = sqlx::query!(
    r#"
    WITH range_txs AS (
        SELECT t.transacted_at::date AS tx_date, t.currency
        FROM transactions t
        WHERE t.user_id = $1
          AND t.kind IN ('Income', 'Expense')
          AND t.transacted_at >= $2
          AND t.transacted_at <  $3
    ),
    distinct_pairs AS (SELECT DISTINCT tx_date, currency FROM range_txs)
    SELECT dp.tx_date, dp.currency
    FROM distinct_pairs dp
    WHERE dp.currency != $4
      AND NOT (dp.currency = 'UAH' AND $4 = 'UAH')
      AND (
        (SELECT rate FROM fx_rates
         WHERE from_currency = dp.currency AND to_currency = 'UAH'
           AND rate_date <= dp.tx_date
         ORDER BY rate_date DESC LIMIT 1) IS NULL
        OR
        ($4 != 'UAH' AND (SELECT rate FROM fx_rates
            WHERE from_currency = $4 AND to_currency = 'UAH'
              AND rate_date <= dp.tx_date
            ORDER BY rate_date DESC LIMIT 1) IS NULL)
      )
    LIMIT 10
    "#,
    range.user_id,
    range.from,
    range.to,
    base,
)
.fetch_all(&self.pool)
.await?;

let missing = missing_rows
    .into_iter()
    .map(|r| MissingRate {
        date: r.tx_date,
        currency: r.currency,
    })
    .collect();

Ok((points, missing))
```

(The two-query split — main aggregation + missing rates lookup — is used
deliberately: the main query needs `query_as` because we interpolate
`granularity`, while the missing-rates query can use `query!` for compile
checks.)

- [ ] **Step 2: Prepare sqlx and build**

```bash
cargo sqlx prepare -- --tests 2>&1 | tail -10
cargo build 2>&1 | tail -20
```

Expected: compiles cleanly.

- [ ] **Step 3: Commit**

```bash
git add src/infrastructure/stats_repository.rs .sqlx/
git commit -m "feat(infra): cashflow aggregation in PgStatsRepository"
```

---

## Task 14: Balance-history aggregation

**Files:**
- Modify: `src/infrastructure/stats_repository.rs`

- [ ] **Step 1: Implement `balance_history`**

The query has three concerns:

1. Per-bucket signed delta from transactions (Income/Sell/StakingReward
   positive, Expense/Buy negative; Transfer per the rule from Task 0).
2. Initial balance per account converted to base currency at the account's
   `created_at` date (this is the seed value).
3. Running total: seed + cumulative `SUM(delta) OVER (ORDER BY bucket)`.

Pseudo-SQL (adapt to the rule from Task 0 for Transfer handling):

```rust
let base = range.base_currency.to_uppercase();
let gran = granularity.as_sql();
let bucket_expr = format!("date_trunc('{}', t.transacted_at)", gran);

let sql = format!(
    r#"
    WITH signed_txs AS (
        SELECT t.user_id,
               t.transacted_at,
               t.transacted_at::date AS tx_date,
               t.currency,
               CASE
                   WHEN t.kind IN ('Income','Sell','StakingReward') THEN t.amount
                   WHEN t.kind IN ('Expense','Buy')                 THEN -t.amount
                   WHEN t.kind = 'Transfer'                          THEN <RULE FROM TASK 0>
                   ELSE 0
               END AS signed_amount,
               {bucket_expr} AS bucket
        FROM transactions t
        WHERE t.user_id = $1 AND t.transacted_at < $3
    ),
    rates AS (
        SELECT s.tx_date, s.currency,
            (SELECT rate FROM fx_rates
             WHERE from_currency = s.currency AND to_currency = 'UAH'
               AND rate_date <= s.tx_date
             ORDER BY rate_date DESC LIMIT 1) AS to_uah,
            (SELECT rate FROM fx_rates
             WHERE from_currency = $4 AND to_currency = 'UAH'
               AND rate_date <= s.tx_date
             ORDER BY rate_date DESC LIMIT 1) AS base_to_uah
        FROM (SELECT DISTINCT tx_date, currency FROM signed_txs) s
    ),
    converted AS (
        SELECT st.bucket, st.transacted_at,
               <CONVERSION CASE OVER signed_amount, st.currency, $4, r.to_uah, r.base_to_uah>
               AS delta_in_base
        FROM signed_txs st
        LEFT JOIN rates r ON r.tx_date = st.tx_date AND r.currency = st.currency
    ),
    bucket_deltas AS (
        SELECT bucket, COALESCE(SUM(delta_in_base), 0) AS delta
        FROM converted
        GROUP BY bucket
    ),
    -- Seed: sum of (account.balance) converted using account creation date rates
    seed AS (
        SELECT COALESCE(SUM(
            <CONVERSION CASE for accounts.balance, accounts.currency, $4 at accounts.created_at::date>
        ), 0) AS initial
        FROM accounts WHERE user_id = $1
    ),
    pre_range AS (
        -- Sum of converted deltas for transactions strictly before $2
        SELECT COALESCE(SUM(c.delta_in_base), 0) AS pre_total
        FROM converted c
        WHERE c.transacted_at < $2
    ),
    bucketed AS (
        SELECT bucket, delta
        FROM bucket_deltas
        WHERE bucket >= date_trunc('{gran}', $2)
        ORDER BY bucket
    )
    SELECT
        bucket AS "bucket!",
        ((SELECT initial FROM seed) + (SELECT pre_total FROM pre_range)
            + SUM(delta) OVER (ORDER BY bucket)) AS balance
    FROM bucketed
    "#
);
```

Replace each `<...>` placeholder with the explicit `CASE` expression
following the same conversion rule used in `cashflow` and `categories`.

(The exact text repeats the conversion `CASE` in three places — that's
fine for now; once all endpoints exist, factor into a SQL helper or
PL/pgSQL function in a follow-up.)

For the missing-rates list, run a separate query similar to the cashflow
missing-rates query but covering all transaction kinds and including the
account creation date rates.

- [ ] **Step 2: Prepare sqlx and build**

```bash
cargo sqlx prepare -- --tests 2>&1 | tail -10
cargo build 2>&1 | tail -20
```

Expected: compiles cleanly.

- [ ] **Step 3: Commit**

```bash
git add src/infrastructure/stats_repository.rs .sqlx/
git commit -m "feat(infra): balance-history aggregation"
```

---

## Task 15: Investment trade legs query

**Files:**
- Modify: `src/infrastructure/stats_repository.rs`

- [ ] **Step 1: Implement `investment_trades`**

Returns each trade leg in chronological order per ticker (Buy, Sell,
StakingReward), with `amount` already converted to the base currency.

```rust
let base = base_currency.to_uppercase();

let rows = sqlx::query!(
    r#"
    WITH legs AS (
        SELECT td.ticker,
               t.kind,
               td.quantity,
               t.amount,
               t.currency,
               t.transacted_at,
               t.transacted_at::date AS tx_date
        FROM transactions t
        JOIN trade_details td ON td.transaction_id = t.id
        WHERE t.user_id = $1
          AND t.kind IN ('Buy', 'Sell', 'StakingReward')
    ),
    rates AS (
        SELECT l.tx_date, l.currency,
            (SELECT rate FROM fx_rates
             WHERE from_currency = l.currency AND to_currency = 'UAH'
               AND rate_date <= l.tx_date
             ORDER BY rate_date DESC LIMIT 1) AS to_uah,
            (SELECT rate FROM fx_rates
             WHERE from_currency = $2 AND to_currency = 'UAH'
               AND rate_date <= l.tx_date
             ORDER BY rate_date DESC LIMIT 1) AS base_to_uah
        FROM (SELECT DISTINCT tx_date, currency FROM legs) l
    )
    SELECT l.ticker, l.kind, l.quantity, l.transacted_at,
           l.tx_date, l.currency,
           CASE
               WHEN l.currency = $2 THEN l.amount
               WHEN l.currency = 'UAH' AND $2 = 'UAH' THEN l.amount
               WHEN l.currency = 'UAH' AND r.base_to_uah IS NOT NULL
                   THEN l.amount / r.base_to_uah
               WHEN $2 = 'UAH' AND r.to_uah IS NOT NULL
                   THEN l.amount * r.to_uah
               WHEN r.to_uah IS NOT NULL AND r.base_to_uah IS NOT NULL
                   THEN l.amount * r.to_uah / r.base_to_uah
               ELSE NULL
           END AS amount_in_base
    FROM legs l
    LEFT JOIN rates r ON r.tx_date = l.tx_date AND r.currency = l.currency
    ORDER BY l.ticker, l.transacted_at
    "#,
    user_id,
    base,
)
.fetch_all(&self.pool)
.await?;

let mut legs = Vec::with_capacity(rows.len());
let mut missing: Vec<MissingRate> = Vec::new();
for r in rows {
    match r.amount_in_base {
        Some(amt) => legs.push(TickerTradeLeg {
            ticker: r.ticker,
            kind: r.kind,
            quantity: r.quantity,
            amount_in_base: amt,
            transacted_at: r.transacted_at,
        }),
        None => {
            if missing.len() < 10 {
                missing.push(MissingRate {
                    date: r.tx_date,
                    currency: r.currency,
                });
            }
        }
    }
}

Ok((legs, missing))
```

- [ ] **Step 2: Prepare sqlx and build**

```bash
cargo sqlx prepare -- --tests 2>&1 | tail -10
cargo build 2>&1 | tail -20
```

Expected: compiles cleanly.

- [ ] **Step 3: Commit**

```bash
git add src/infrastructure/stats_repository.rs .sqlx/
git commit -m "feat(infra): investment trade legs aggregation"
```

---

## Task 16: Dashboard aggregation

**Files:**
- Modify: `src/infrastructure/stats_repository.rs`

The dashboard is a composition. The simplest implementation calls the
other repo methods internally; that's fine and keeps logic in one place.

- [ ] **Step 1: Implement `dashboard`**

```rust
async fn dashboard(
    &self,
    user_id: Uuid,
    base_currency: &str,
    top_n: i64,
) -> anyhow::Result<DashboardStats> {
    use chrono::{Datelike, TimeZone};

    let now = chrono::Utc::now();
    let month_start = chrono::Utc
        .with_ymd_and_hms(now.year(), now.month(), 1, 0, 0, 0)
        .unwrap();
    let month_end = if now.month() == 12 {
        chrono::Utc
            .with_ymd_and_hms(now.year() + 1, 1, 1, 0, 0, 0)
            .unwrap()
    } else {
        chrono::Utc
            .with_ymd_and_hms(now.year(), now.month() + 1, 1, 0, 0, 0)
            .unwrap()
    };

    // Net worth: balance_history with granularity=Year over [-∞, now]
    // is overkill; use a simpler dedicated query: sum(seed + signed deltas
    // up to now). For v1, derive from the balance_history call with a
    // sentinel range.
    let range = StatsRange {
        user_id,
        from: chrono::Utc.with_ymd_and_hms(1970, 1, 1, 0, 0, 0).unwrap(),
        to: now,
        base_currency: base_currency.to_string(),
    };
    let (history, mut missing) = self.balance_history(&range, Granularity::Year).await?;
    let net_worth = history
        .last()
        .map(|p| p.balance)
        .unwrap_or(Decimal::ZERO);

    // Month income/expense
    let month_range = StatsRange {
        user_id,
        from: month_start,
        to: month_end,
        base_currency: base_currency.to_string(),
    };
    let (cashflow, missing_cf) =
        self.cashflow(&month_range, Granularity::Month).await?;
    let month_income = cashflow
        .iter()
        .map(|p| p.income)
        .fold(Decimal::ZERO, |a, b| a + b);
    let month_expense = cashflow
        .iter()
        .map(|p| p.expense)
        .fold(Decimal::ZERO, |a, b| a + b);
    missing.extend(missing_cf);

    // Top N expense categories this month
    let (categories, missing_cat) = self.categories(&month_range, "Expense").await?;
    let mut top_categories = categories;
    top_categories.truncate(top_n.max(0) as usize);
    missing.extend(missing_cat);

    // Dedup missing (date, currency)
    missing.sort_by(|a, b| a.date.cmp(&b.date).then(a.currency.cmp(&b.currency)));
    missing.dedup();
    if missing.len() > 10 {
        missing.truncate(10);
    }

    Ok(DashboardStats {
        net_worth,
        month_income,
        month_expense,
        top_categories,
        missing_rates: missing,
    })
}
```

- [ ] **Step 2: Build**

```bash
cargo build 2>&1 | tail -20
```

Expected: compiles cleanly.

- [ ] **Step 3: Commit**

```bash
git add src/infrastructure/stats_repository.rs
git commit -m "feat(infra): dashboard aggregation composes other stats"
```

---

## Task 17: `StatsService` (with average-cost realized P&L)

**Files:**
- Create: `src/application/stats.rs`
- Modify: `src/application/mod.rs`

- [ ] **Step 1: Write the service with unit tests for avg-cost math**

```rust
use std::collections::BTreeMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use uuid::Uuid;

use crate::application::user_settings::UserSettingsService;
use crate::domain::error::DomainError;
use crate::domain::fx_rate::FxRateRepository;
use crate::domain::stats::{
    BalanceHistoryPoint, CashflowPoint, CategoryBreakdownItem, DashboardStats, Granularity,
    MissingRate, StatsRange, StatsRepository, TickerHolding, TickerTradeLeg,
};

pub struct StatsService {
    repo: Arc<dyn StatsRepository>,
    fx_repo: Arc<dyn FxRateRepository>,
    user_settings: Arc<UserSettingsService>,
}

impl StatsService {
    pub fn new(
        repo: Arc<dyn StatsRepository>,
        fx_repo: Arc<dyn FxRateRepository>,
        user_settings: Arc<UserSettingsService>,
    ) -> Self {
        Self {
            repo,
            fx_repo,
            user_settings,
        }
    }

    /// Returns the resolved base currency given an optional override.
    pub async fn resolve_base_currency(
        &self,
        user_id: Uuid,
        override_value: Option<String>,
    ) -> anyhow::Result<String> {
        let candidate = match override_value {
            Some(v) => v,
            None => self.user_settings.get_or_default(user_id).await?.base_currency,
        };
        let normalized = candidate.to_uppercase();
        if normalized.len() != 3 {
            return Err(DomainError::InvalidInput(
                "base_currency must be a 3-letter ISO code".to_string(),
            )
            .into());
        }
        if normalized != "UAH" {
            let known = self.fx_repo.known_currencies().await?;
            if !known.iter().any(|c| c == &normalized) {
                return Err(DomainError::InvalidInput(format!(
                    "unknown currency: {normalized}"
                ))
                .into());
            }
        }
        Ok(normalized)
    }

    pub async fn dashboard(
        &self,
        user_id: Uuid,
        base_currency: Option<String>,
        top_n: i64,
    ) -> anyhow::Result<(String, DashboardStats)> {
        let base = self.resolve_base_currency(user_id, base_currency).await?;
        let stats = self.repo.dashboard(user_id, &base, top_n).await?;
        Ok((base, stats))
    }

    pub async fn balance_history(
        &self,
        user_id: Uuid,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
        granularity: Granularity,
        base_currency: Option<String>,
    ) -> anyhow::Result<(String, Vec<BalanceHistoryPoint>, Vec<MissingRate>)> {
        let base = self.resolve_base_currency(user_id, base_currency).await?;
        let range = StatsRange {
            user_id,
            from,
            to,
            base_currency: base.clone(),
        };
        let (points, missing) = self.repo.balance_history(&range, granularity).await?;
        Ok((base, points, missing))
    }

    pub async fn cashflow(
        &self,
        user_id: Uuid,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
        granularity: Granularity,
        base_currency: Option<String>,
    ) -> anyhow::Result<(String, Vec<CashflowPoint>, Vec<MissingRate>)> {
        let base = self.resolve_base_currency(user_id, base_currency).await?;
        let range = StatsRange {
            user_id,
            from,
            to,
            base_currency: base.clone(),
        };
        let (points, missing) = self.repo.cashflow(&range, granularity).await?;
        Ok((base, points, missing))
    }

    pub async fn categories(
        &self,
        user_id: Uuid,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
        kind: &str,
        base_currency: Option<String>,
    ) -> anyhow::Result<(String, Vec<CategoryBreakdownItem>, Vec<MissingRate>)> {
        if kind != "Expense" && kind != "Income" {
            return Err(DomainError::InvalidInput(format!("unknown kind: {kind}")).into());
        }
        let base = self.resolve_base_currency(user_id, base_currency).await?;
        let range = StatsRange {
            user_id,
            from,
            to,
            base_currency: base.clone(),
        };
        let (items, missing) = self.repo.categories(&range, kind).await?;
        Ok((base, items, missing))
    }

    pub async fn investments(
        &self,
        user_id: Uuid,
        base_currency: Option<String>,
    ) -> anyhow::Result<(String, Vec<TickerHolding>, Vec<MissingRate>)> {
        let base = self.resolve_base_currency(user_id, base_currency).await?;
        let (legs, missing) = self.repo.investment_trades(user_id, &base).await?;
        let holdings = compute_holdings(legs);
        Ok((base, holdings, missing))
    }
}

/// Walk per-ticker chronological legs maintaining (qty_held, total_cost).
/// Sells reduce both proportionally and accumulate realized P&L using
/// the average cost at sell time. StakingReward increases qty without
/// adding cost.
fn compute_holdings(legs: Vec<TickerTradeLeg>) -> Vec<TickerHolding> {
    let mut by_ticker: BTreeMap<String, Vec<TickerTradeLeg>> = BTreeMap::new();
    for l in legs {
        by_ticker.entry(l.ticker.clone()).or_default().push(l);
    }

    let mut out = Vec::with_capacity(by_ticker.len());
    for (ticker, legs) in by_ticker {
        let mut qty_held = Decimal::ZERO;
        let mut total_cost = Decimal::ZERO;
        let mut realized = Decimal::ZERO;
        let mut staking_received = Decimal::ZERO;

        for l in legs {
            match l.kind.as_str() {
                "Buy" => {
                    qty_held += l.quantity;
                    total_cost += l.amount_in_base;
                }
                "Sell" => {
                    if qty_held > Decimal::ZERO {
                        let avg_cost = total_cost / qty_held;
                        let cost_of_sold = avg_cost * l.quantity;
                        realized += l.amount_in_base - cost_of_sold;
                        let new_qty = qty_held - l.quantity;
                        if new_qty <= Decimal::ZERO {
                            qty_held = Decimal::ZERO;
                            total_cost = Decimal::ZERO;
                        } else {
                            total_cost = avg_cost * new_qty;
                            qty_held = new_qty;
                        }
                    } else {
                        // Sell with no holdings — short or data error.
                        // Treat the proceeds as pure realized gain to keep
                        // the math from blowing up; flag is implicit.
                        realized += l.amount_in_base;
                    }
                }
                "StakingReward" => {
                    qty_held += l.quantity;
                    staking_received += l.quantity;
                }
                _ => {}
            }
        }

        out.push(TickerHolding {
            ticker,
            holdings: qty_held,
            cost_basis: total_cost,
            realized_pnl: realized,
            staking_received,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn leg(ticker: &str, kind: &str, qty: Decimal, amount: Decimal) -> TickerTradeLeg {
        TickerTradeLeg {
            ticker: ticker.to_string(),
            kind: kind.to_string(),
            quantity: qty,
            amount_in_base: amount,
            transacted_at: Utc::now(),
        }
    }

    #[test]
    fn average_cost_realized_pnl_one_buy_one_sell() {
        // Buy 10 @ 100 (total 1000), Sell 4 @ 150 (proceeds 600)
        // avg cost = 100, cost of sold = 400, realized = 200
        // remaining qty = 6, remaining cost = 600
        let legs = vec![
            leg("BTC", "Buy",  Decimal::new(10, 0), Decimal::new(1000, 0)),
            leg("BTC", "Sell", Decimal::new(4, 0),  Decimal::new(600, 0)),
        ];
        let h = compute_holdings(legs);
        assert_eq!(h.len(), 1);
        assert_eq!(h[0].holdings, Decimal::new(6, 0));
        assert_eq!(h[0].cost_basis, Decimal::new(600, 0));
        assert_eq!(h[0].realized_pnl, Decimal::new(200, 0));
        assert_eq!(h[0].staking_received, Decimal::ZERO);
    }

    #[test]
    fn staking_reward_increases_holdings_not_cost() {
        let legs = vec![
            leg("ETH", "Buy",            Decimal::new(2, 0), Decimal::new(4000, 0)),
            leg("ETH", "StakingReward",  Decimal::new(1, 1), Decimal::ZERO),
        ];
        let h = compute_holdings(legs);
        assert_eq!(h[0].holdings, Decimal::new(21, 1)); // 2.1
        assert_eq!(h[0].cost_basis, Decimal::new(4000, 0));
        assert_eq!(h[0].staking_received, Decimal::new(1, 1)); // 0.1
    }

    #[test]
    fn two_buys_average_costs() {
        // Buy 1 @ 100, Buy 1 @ 200, Sell 1 @ 250
        // avg cost = 150, realized = 100, remaining qty=1, remaining cost=150
        let legs = vec![
            leg("X", "Buy",  Decimal::new(1, 0), Decimal::new(100, 0)),
            leg("X", "Buy",  Decimal::new(1, 0), Decimal::new(200, 0)),
            leg("X", "Sell", Decimal::new(1, 0), Decimal::new(250, 0)),
        ];
        let h = compute_holdings(legs);
        assert_eq!(h[0].holdings, Decimal::new(1, 0));
        assert_eq!(h[0].cost_basis, Decimal::new(150, 0));
        assert_eq!(h[0].realized_pnl, Decimal::new(100, 0));
    }
}
```

Add `pub mod stats;` to `src/application/mod.rs`.

- [ ] **Step 2: Run unit tests**

```bash
cargo test --lib application::stats 2>&1 | tail -30
```

Expected: 3 tests pass.

- [ ] **Step 3: Commit**

```bash
git add src/application/stats.rs src/application/mod.rs
git commit -m "feat(application): StatsService with average-cost realized P&L"
```

---

## Task 18: API DTOs and stats handlers

**Files:**
- Modify: `src/api/dto.rs`
- Create: `src/api/handlers/stats.rs`
- Modify: `src/api/handlers/mod.rs`, `src/api/state.rs`, `src/api/routes.rs`

- [ ] **Step 1: DTOs**

In `src/api/dto.rs` append:

```rust
#[derive(Deserialize)]
pub struct StatsRangeQuery {
    pub from: Option<i64>,
    pub to: Option<i64>,
    pub base_currency: Option<String>,
}

#[derive(Deserialize)]
pub struct StatsGranularityQuery {
    pub from: Option<i64>,
    pub to: Option<i64>,
    pub base_currency: Option<String>,
    pub granularity: Option<String>, // "day" | "month" | "year"
}

#[derive(Deserialize)]
pub struct CategoriesQuery {
    pub from: Option<i64>,
    pub to: Option<i64>,
    pub base_currency: Option<String>,
    pub kind: Option<String>, // "Expense" | "Income"
}

#[derive(Deserialize)]
pub struct DashboardQuery {
    pub base_currency: Option<String>,
    pub top_n: Option<i64>,
}

#[derive(Serialize)]
pub struct MissingRateDto {
    pub date: String,
    pub currency: String,
}

#[derive(Serialize)]
pub struct DashboardResponse {
    pub base_currency: String,
    pub net_worth: Decimal,
    pub month_income: Decimal,
    pub month_expense: Decimal,
    pub top_categories: Vec<CategoryBreakdownDto>,
    pub partial: bool,
    pub missing_rates: Vec<MissingRateDto>,
}

#[derive(Serialize)]
pub struct CategoryBreakdownDto {
    pub category_id: Option<Uuid>,
    pub name: String,
    pub total: Decimal,
}

#[derive(Serialize)]
pub struct BalanceHistoryPointDto {
    pub period_start: i64,
    pub balance: Decimal,
}

#[derive(Serialize)]
pub struct BalanceHistoryResponse {
    pub base_currency: String,
    pub granularity: String,
    pub points: Vec<BalanceHistoryPointDto>,
    pub partial: bool,
    pub missing_rates: Vec<MissingRateDto>,
}

#[derive(Serialize)]
pub struct CashflowPointDto {
    pub period_start: i64,
    pub income: Decimal,
    pub expense: Decimal,
}

#[derive(Serialize)]
pub struct CashflowResponse {
    pub base_currency: String,
    pub granularity: String,
    pub points: Vec<CashflowPointDto>,
    pub partial: bool,
    pub missing_rates: Vec<MissingRateDto>,
}

#[derive(Serialize)]
pub struct CategoriesResponse {
    pub base_currency: String,
    pub kind: String,
    pub items: Vec<CategoryBreakdownDto>,
    pub partial: bool,
    pub missing_rates: Vec<MissingRateDto>,
}

#[derive(Serialize)]
pub struct TickerHoldingDto {
    pub ticker: String,
    pub holdings: Decimal,
    pub cost_basis: Decimal,
    pub realized_pnl: Decimal,
    pub staking_received: Decimal,
}

#[derive(Serialize)]
pub struct InvestmentsResponse {
    pub base_currency: String,
    pub tickers: Vec<TickerHoldingDto>,
    pub partial: bool,
    pub missing_rates: Vec<MissingRateDto>,
}
```

- [ ] **Step 2: Handlers**

`src/api/handlers/stats.rs`:

```rust
use axum::Json;
use axum::extract::{Extension, Query, State};
use axum::http::{HeaderMap, HeaderValue, header};
use axum::response::IntoResponse;
use chrono::{DateTime, TimeZone, Utc};

use crate::api::dto::{
    BalanceHistoryPointDto, BalanceHistoryResponse, CashflowPointDto, CashflowResponse,
    CategoriesQuery, CategoriesResponse, CategoryBreakdownDto, DashboardQuery,
    DashboardResponse, InvestmentsResponse, MissingRateDto, StatsGranularityQuery,
    StatsRangeQuery, TickerHoldingDto,
};
use crate::api::error::AppError;
use crate::api::middleware::AuthUser;
use crate::api::state::AppState;
use crate::domain::error::DomainError;
use crate::domain::stats::{Granularity, MissingRate};

const CACHE_HEADER: &str = "private, max-age=60";

fn cache_headers() -> HeaderMap {
    let mut h = HeaderMap::new();
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static(CACHE_HEADER));
    h
}

fn parse_range(
    from: Option<i64>,
    to: Option<i64>,
) -> Result<(DateTime<Utc>, DateTime<Utc>), DomainError> {
    let now = Utc::now();
    let from_dt = match from {
        Some(s) => DateTime::<Utc>::from_timestamp(s, 0)
            .ok_or_else(|| DomainError::InvalidInput("from out of range".into()))?,
        None => Utc.with_ymd_and_hms(1970, 1, 1, 0, 0, 0).unwrap(),
    };
    let to_dt = match to {
        Some(s) => DateTime::<Utc>::from_timestamp(s, 0)
            .ok_or_else(|| DomainError::InvalidInput("to out of range".into()))?,
        None => now,
    };
    if from_dt > to_dt {
        return Err(DomainError::InvalidInput("from must be <= to".into()));
    }
    Ok((from_dt, to_dt))
}

fn parse_granularity(s: Option<String>) -> Result<Granularity, DomainError> {
    let s = s.unwrap_or_else(|| "month".to_string());
    Granularity::from_str(&s)
        .ok_or_else(|| DomainError::InvalidInput(format!("unknown granularity: {s}")))
}

fn missing_to_dto(m: Vec<MissingRate>) -> Vec<MissingRateDto> {
    m.into_iter()
        .map(|x| MissingRateDto {
            date: x.date.format("%Y-%m-%d").to_string(),
            currency: x.currency,
        })
        .collect()
}

pub async fn dashboard(
    State(state): State<AppState>,
    Extension(AuthUser(user_id)): Extension<AuthUser>,
    Query(q): Query<DashboardQuery>,
) -> Result<impl IntoResponse, AppError> {
    let top_n = q.top_n.unwrap_or(5);
    let (base, stats) = state.stats.dashboard(user_id, q.base_currency, top_n).await?;
    let partial = !stats.missing_rates.is_empty();
    let body = DashboardResponse {
        base_currency: base,
        net_worth: stats.net_worth,
        month_income: stats.month_income,
        month_expense: stats.month_expense,
        top_categories: stats
            .top_categories
            .into_iter()
            .map(|c| CategoryBreakdownDto {
                category_id: c.category_id,
                name: c.name,
                total: c.total,
            })
            .collect(),
        partial,
        missing_rates: missing_to_dto(stats.missing_rates),
    };
    Ok((cache_headers(), Json(body)))
}

pub async fn balance_history(
    State(state): State<AppState>,
    Extension(AuthUser(user_id)): Extension<AuthUser>,
    Query(q): Query<StatsGranularityQuery>,
) -> Result<impl IntoResponse, AppError> {
    let (from, to) = parse_range(q.from, q.to)?;
    let granularity = parse_granularity(q.granularity)?;
    let (base, points, missing) = state
        .stats
        .balance_history(user_id, from, to, granularity, q.base_currency)
        .await?;
    let partial = !missing.is_empty();
    let body = BalanceHistoryResponse {
        base_currency: base,
        granularity: granularity.as_sql().to_string(),
        points: points
            .into_iter()
            .map(|p| BalanceHistoryPointDto {
                period_start: p.period_start.timestamp(),
                balance: p.balance,
            })
            .collect(),
        partial,
        missing_rates: missing_to_dto(missing),
    };
    Ok((cache_headers(), Json(body)))
}

pub async fn cashflow(
    State(state): State<AppState>,
    Extension(AuthUser(user_id)): Extension<AuthUser>,
    Query(q): Query<StatsGranularityQuery>,
) -> Result<impl IntoResponse, AppError> {
    let (from, to) = parse_range(q.from, q.to)?;
    let granularity = parse_granularity(q.granularity)?;
    let (base, points, missing) = state
        .stats
        .cashflow(user_id, from, to, granularity, q.base_currency)
        .await?;
    let partial = !missing.is_empty();
    let body = CashflowResponse {
        base_currency: base,
        granularity: granularity.as_sql().to_string(),
        points: points
            .into_iter()
            .map(|p| CashflowPointDto {
                period_start: p.period_start.timestamp(),
                income: p.income,
                expense: p.expense,
            })
            .collect(),
        partial,
        missing_rates: missing_to_dto(missing),
    };
    Ok((cache_headers(), Json(body)))
}

pub async fn categories(
    State(state): State<AppState>,
    Extension(AuthUser(user_id)): Extension<AuthUser>,
    Query(q): Query<CategoriesQuery>,
) -> Result<impl IntoResponse, AppError> {
    let (from, to) = parse_range(q.from, q.to)?;
    let kind = q.kind.unwrap_or_else(|| "Expense".to_string());
    let (base, items, missing) = state
        .stats
        .categories(user_id, from, to, &kind, q.base_currency)
        .await?;
    let partial = !missing.is_empty();
    let body = CategoriesResponse {
        base_currency: base,
        kind,
        items: items
            .into_iter()
            .map(|c| CategoryBreakdownDto {
                category_id: c.category_id,
                name: c.name,
                total: c.total,
            })
            .collect(),
        partial,
        missing_rates: missing_to_dto(missing),
    };
    Ok((cache_headers(), Json(body)))
}

pub async fn investments(
    State(state): State<AppState>,
    Extension(AuthUser(user_id)): Extension<AuthUser>,
    Query(q): Query<StatsRangeQuery>,
) -> Result<impl IntoResponse, AppError> {
    let (base, tickers, missing) = state.stats.investments(user_id, q.base_currency).await?;
    let partial = !missing.is_empty();
    let body = InvestmentsResponse {
        base_currency: base,
        tickers: tickers
            .into_iter()
            .map(|t| TickerHoldingDto {
                ticker: t.ticker,
                holdings: t.holdings,
                cost_basis: t.cost_basis,
                realized_pnl: t.realized_pnl,
                staking_received: t.staking_received,
            })
            .collect(),
        partial,
        missing_rates: missing_to_dto(missing),
    };
    Ok((cache_headers(), Json(body)))
}
```

Add `pub mod stats;` to `src/api/handlers/mod.rs`.

- [ ] **Step 3: AppState**

In `src/api/state.rs`:

```rust
use crate::application::stats::StatsService;
// inside AppState:
pub stats: Arc<StatsService>,
```

- [ ] **Step 4: Routes**

In `src/api/routes.rs`, inside the protected router (alphabetical-ish placement):

```rust
.route("/stats/dashboard",        get(crate::api::handlers::stats::dashboard))
.route("/stats/balance-history",  get(crate::api::handlers::stats::balance_history))
.route("/stats/cashflow",         get(crate::api::handlers::stats::cashflow))
.route("/stats/categories",       get(crate::api::handlers::stats::categories))
.route("/stats/investments",      get(crate::api::handlers::stats::investments))
```

- [ ] **Step 5: Build**

```bash
cargo build 2>&1 | tail -20
```

Expected: compiles cleanly.

- [ ] **Step 6: Commit**

```bash
git add src/api/dto.rs src/api/handlers/stats.rs src/api/handlers/mod.rs \
       src/api/state.rs src/api/routes.rs
git commit -m "feat(api): /stats handlers, DTOs, routes"
```

---

## Task 19: Wire StatsService and FX sync background task in `main.rs`

**Files:**
- Modify: `src/main.rs`

- [ ] **Step 1: Wire `StatsService`**

After creating `fx_repo` and `user_settings_repo` (added in Task 11):

```rust
let stats_repo: Arc<dyn moneykeeper::domain::stats::StatsRepository> = Arc::new(
    moneykeeper::infrastructure::stats_repository::PgStatsRepository::new(pool.clone()),
);

let user_settings_service = Arc::new(
    moneykeeper::application::user_settings::UserSettingsService::new(
        Arc::clone(&user_settings_repo),
        Arc::clone(&fx_repo),
    ),
);

let stats_service = Arc::new(moneykeeper::application::stats::StatsService::new(
    Arc::clone(&stats_repo),
    Arc::clone(&fx_repo),
    Arc::clone(&user_settings_service),
));
```

Update the `AppState { ... }` literal to use `user_settings_service` and to
add `stats: Arc::clone(&stats_service)`.

- [ ] **Step 2: Spawn FX sync task**

After `axum::serve` is set up but before the `.await?` (or instead spawn
before that line and use `tokio::select!` if you prefer — simplest is to
spawn fire-and-forget):

```rust
{
    let nbu = Arc::new(moneykeeper::infrastructure::nbu_client::NbuFxRateSource::new());
    let fx_sync = Arc::new(moneykeeper::application::fx_sync::FxSyncUseCase::new(
        nbu,
        Arc::clone(&fx_repo),
    ));
    tokio::spawn(async move {
        // Initial sync on startup
        if let Err(e) = fx_sync.sync_today().await {
            tracing::warn!("initial fx sync failed: {e:#}");
        }
        // Daily loop
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(24 * 60 * 60)).await;
            if let Err(e) = fx_sync.sync_today().await {
                tracing::warn!("daily fx sync failed: {e:#}");
            }
        }
    });
}
```

- [ ] **Step 3: Build and run smoke check**

```bash
cargo build 2>&1 | tail -20
```

Expected: compiles cleanly. (Don't run the binary — that requires DB and
JWKS env. The integration tests cover it.)

- [ ] **Step 4: Commit**

```bash
git add src/main.rs
git commit -m "feat: wire StatsService and spawn FX sync background task"
```

---

## Task 20: Integration tests — categories endpoint

**Files:**
- Modify: `tests/api/stats.rs`

- [ ] **Step 1: Helper to seed transactions and categories**

Reuse existing helpers if they exist (read `tests/api/helpers.rs` and
`tests/api/transactions.rs` to find them); otherwise add a small helper:

```rust
pub async fn seed_account_and_transaction(
    pool: &sqlx::PgPool,
    user_id: uuid::Uuid,
    currency: &str,
    amount: rust_decimal::Decimal,
    kind: &str,
    category_id: Option<uuid::Uuid>,
    transacted_at: chrono::DateTime<chrono::Utc>,
) -> uuid::Uuid {
    let account_id = uuid::Uuid::new_v4();
    sqlx::query!(
        "INSERT INTO accounts (id, user_id, name, account_type, currency, balance, created_at, updated_at)
         VALUES ($1, $2, 'test', 'Cash', $3, 0, $4, $4)",
        account_id, user_id, currency, transacted_at,
    ).execute(pool).await.unwrap();

    let tx_id = uuid::Uuid::new_v4();
    sqlx::query!(
        "INSERT INTO transactions (id, account_id, user_id, amount, currency, kind, category_id, transacted_at, created_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $8)",
        tx_id, account_id, user_id, amount, currency, kind, category_id, transacted_at,
    ).execute(pool).await.unwrap();
    tx_id
}
```

(Adapt column lists to match the actual existing schema — verify against
the migrations.)

- [ ] **Step 2: Add tests**

```rust
#[tokio::test]
async fn categories_endpoint_groups_by_category() {
    let db = TestPostgres::new().await;
    let app = build_app(db.pool.clone()).await;
    let (token, user_id) = auth_token();

    let cat = uuid::Uuid::new_v4();
    sqlx::query!(
        "INSERT INTO categories (id, user_id, name, color, created_at)
         VALUES ($1, $2, 'Groceries', NULL, NOW())",
        cat, user_id,
    ).execute(&db.pool).await.unwrap();

    let now = chrono::Utc::now();
    seed_account_and_transaction(&db.pool, user_id, "UAH", rust_decimal::Decimal::new(500, 0), "Expense", Some(cat), now).await;
    seed_account_and_transaction(&db.pool, user_id, "UAH", rust_decimal::Decimal::new(300, 0), "Expense", None, now).await;

    let response = app
        .get("/stats/categories")
        .add_header("authorization", format!("Bearer {token}"))
        .await;

    response.assert_status_ok();
    let body: serde_json::Value = response.json();
    assert_eq!(body["base_currency"], "UAH");
    let items = body["items"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["name"], "Groceries");
    assert_eq!(items[0]["total"], "500");
    assert_eq!(items[1]["name"], "Uncategorized");
}

#[tokio::test]
async fn categories_endpoint_converts_to_base_currency() {
    let db = TestPostgres::new().await;
    let date = chrono::NaiveDate::from_ymd_opt(2026, 5, 10).unwrap();
    seed_fx_rate(&db.pool, date, "USD", rust_decimal::Decimal::new(40, 0)).await;
    let app = build_app(db.pool.clone()).await;
    let (token, user_id) = auth_token();

    let when = chrono::Utc.with_ymd_and_hms(2026, 5, 10, 12, 0, 0).unwrap();
    seed_account_and_transaction(&db.pool, user_id, "USD", rust_decimal::Decimal::new(100, 0), "Expense", None, when).await;

    let response = app
        .get("/stats/categories")
        .add_header("authorization", format!("Bearer {token}"))
        .await;

    response.assert_status_ok();
    let body: serde_json::Value = response.json();
    assert_eq!(body["base_currency"], "UAH");
    assert_eq!(body["items"][0]["total"], "4000"); // 100 USD * 40 = 4000 UAH
    assert_eq!(body["partial"], false);
}

#[tokio::test]
async fn categories_endpoint_marks_partial_when_rate_missing() {
    let db = TestPostgres::new().await;
    let app = build_app(db.pool.clone()).await;
    let (token, user_id) = auth_token();

    // Need to set base currency to UAH (default) but transactions in EUR with no rate
    let when = chrono::Utc.with_ymd_and_hms(2026, 5, 10, 12, 0, 0).unwrap();
    seed_account_and_transaction(&db.pool, user_id, "EUR", rust_decimal::Decimal::new(100, 0), "Expense", None, when).await;

    let response = app
        .get("/stats/categories?from=1746230400&to=1746576000")
        .add_header("authorization", format!("Bearer {token}"))
        .await;

    response.assert_status_ok();
    let body: serde_json::Value = response.json();
    assert_eq!(body["partial"], true);
    assert!(!body["missing_rates"].as_array().unwrap().is_empty());
}
```

- [ ] **Step 3: Run tests**

```bash
cargo test --test api stats::categories 2>&1 | tail -30
```

Expected: 3 tests pass.

- [ ] **Step 4: Commit**

```bash
git add tests/api/stats.rs tests/api/helpers.rs
git commit -m "test(api): integration tests for /stats/categories"
```

---

## Task 21: Integration tests — cashflow & balance-history

**Files:**
- Modify: `tests/api/stats.rs`

- [ ] **Step 1: Tests**

```rust
#[tokio::test]
async fn cashflow_groups_by_month() {
    let db = TestPostgres::new().await;
    let app = build_app(db.pool.clone()).await;
    let (token, user_id) = auth_token();

    let apr = chrono::Utc.with_ymd_and_hms(2026, 4, 15, 12, 0, 0).unwrap();
    let may = chrono::Utc.with_ymd_and_hms(2026, 5, 15, 12, 0, 0).unwrap();

    seed_account_and_transaction(&db.pool, user_id, "UAH", rust_decimal::Decimal::new(1000, 0), "Income",  None, apr).await;
    seed_account_and_transaction(&db.pool, user_id, "UAH", rust_decimal::Decimal::new(400, 0),  "Expense", None, apr).await;
    seed_account_and_transaction(&db.pool, user_id, "UAH", rust_decimal::Decimal::new(2000, 0), "Income",  None, may).await;

    let from = chrono::Utc.with_ymd_and_hms(2026, 4, 1, 0, 0, 0).unwrap().timestamp();
    let to   = chrono::Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap().timestamp();

    let response = app
        .get(&format!("/stats/cashflow?granularity=month&from={from}&to={to}"))
        .add_header("authorization", format!("Bearer {token}"))
        .await;

    response.assert_status_ok();
    let body: serde_json::Value = response.json();
    let points = body["points"].as_array().unwrap();
    assert_eq!(points.len(), 2);
    assert_eq!(points[0]["income"],  "1000");
    assert_eq!(points[0]["expense"], "400");
    assert_eq!(points[1]["income"],  "2000");
    assert_eq!(points[1]["expense"], "0");
}

#[tokio::test]
async fn balance_history_includes_initial_balance_and_running_total() {
    let db = TestPostgres::new().await;
    let app = build_app(db.pool.clone()).await;
    let (token, user_id) = auth_token();

    // Account created with initial_balance 1000 UAH at 2026-04-01.
    let acc = uuid::Uuid::new_v4();
    let acc_created = chrono::Utc.with_ymd_and_hms(2026, 4, 1, 0, 0, 0).unwrap();
    sqlx::query!(
        "INSERT INTO accounts (id, user_id, name, account_type, currency, balance, created_at, updated_at)
         VALUES ($1, $2, 'test', 'Cash', 'UAH', 1000, $3, $3)",
        acc, user_id, acc_created,
    ).execute(&db.pool).await.unwrap();

    // April: +500 income, -200 expense → +300
    let apr = chrono::Utc.with_ymd_and_hms(2026, 4, 15, 12, 0, 0).unwrap();
    sqlx::query!(
        "INSERT INTO transactions (id, account_id, user_id, amount, currency, kind, transacted_at, created_at)
         VALUES ($1, $2, $3, 500, 'UAH', 'Income', $4, $4)",
        uuid::Uuid::new_v4(), acc, user_id, apr,
    ).execute(&db.pool).await.unwrap();
    sqlx::query!(
        "INSERT INTO transactions (id, account_id, user_id, amount, currency, kind, transacted_at, created_at)
         VALUES ($1, $2, $3, 200, 'UAH', 'Expense', $4, $4)",
        uuid::Uuid::new_v4(), acc, user_id, apr,
    ).execute(&db.pool).await.unwrap();

    let from = chrono::Utc.with_ymd_and_hms(2026, 4, 1, 0, 0, 0).unwrap().timestamp();
    let to   = chrono::Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap().timestamp();

    let response = app
        .get(&format!("/stats/balance-history?granularity=month&from={from}&to={to}"))
        .add_header("authorization", format!("Bearer {token}"))
        .await;

    response.assert_status_ok();
    let body: serde_json::Value = response.json();
    let points = body["points"].as_array().unwrap();
    // Apr: 1000 + 300 = 1300
    assert_eq!(points[0]["balance"], "1300");
}
```

- [ ] **Step 2: Run tests**

```bash
cargo test --test api stats::cashflow stats::balance_history 2>&1 | tail -30
```

Expected: 2 tests pass.

- [ ] **Step 3: Commit**

```bash
git add tests/api/stats.rs
git commit -m "test(api): integration tests for /stats/cashflow and balance-history"
```

---

## Task 22: Integration tests — investments

**Files:**
- Modify: `tests/api/stats.rs`

- [ ] **Step 1: Test**

```rust
#[tokio::test]
async fn investments_computes_holdings_and_realized_pnl() {
    let db = TestPostgres::new().await;
    let app = build_app(db.pool.clone()).await;
    let (token, user_id) = auth_token();

    let acc = uuid::Uuid::new_v4();
    let when_a = chrono::Utc.with_ymd_and_hms(2026, 4, 1, 12, 0, 0).unwrap();
    let when_b = chrono::Utc.with_ymd_and_hms(2026, 5, 1, 12, 0, 0).unwrap();

    sqlx::query!(
        "INSERT INTO accounts (id, user_id, name, account_type, currency, balance, created_at, updated_at)
         VALUES ($1, $2, 'broker', 'Investment', 'UAH', 0, $3, $3)",
        acc, user_id, when_a,
    ).execute(&db.pool).await.unwrap();

    // Buy 10 BTC for 1000 UAH
    let buy_id = uuid::Uuid::new_v4();
    sqlx::query!(
        "INSERT INTO transactions (id, account_id, user_id, amount, currency, kind, transacted_at, created_at)
         VALUES ($1, $2, $3, 1000, 'UAH', 'Buy', $4, $4)",
        buy_id, acc, user_id, when_a,
    ).execute(&db.pool).await.unwrap();
    sqlx::query!(
        "INSERT INTO trade_details (transaction_id, ticker, quantity, price_per_unit, fee)
         VALUES ($1, 'BTC', 10, 100, 0)",
        buy_id,
    ).execute(&db.pool).await.unwrap();

    // Sell 4 BTC for 600 UAH (realized = 600 - 400 = 200)
    let sell_id = uuid::Uuid::new_v4();
    sqlx::query!(
        "INSERT INTO transactions (id, account_id, user_id, amount, currency, kind, transacted_at, created_at)
         VALUES ($1, $2, $3, 600, 'UAH', 'Sell', $4, $4)",
        sell_id, acc, user_id, when_b,
    ).execute(&db.pool).await.unwrap();
    sqlx::query!(
        "INSERT INTO trade_details (transaction_id, ticker, quantity, price_per_unit, fee)
         VALUES ($1, 'BTC', 4, 150, 0)",
        sell_id,
    ).execute(&db.pool).await.unwrap();

    let response = app
        .get("/stats/investments")
        .add_header("authorization", format!("Bearer {token}"))
        .await;

    response.assert_status_ok();
    let body: serde_json::Value = response.json();
    let tickers = body["tickers"].as_array().unwrap();
    assert_eq!(tickers.len(), 1);
    assert_eq!(tickers[0]["ticker"], "BTC");
    assert_eq!(tickers[0]["holdings"], "6");
    assert_eq!(tickers[0]["cost_basis"], "600");
    assert_eq!(tickers[0]["realized_pnl"], "200");
    assert_eq!(tickers[0]["staking_received"], "0");
}
```

- [ ] **Step 2: Run**

```bash
cargo test --test api stats::investments 2>&1 | tail -30
```

Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add tests/api/stats.rs
git commit -m "test(api): integration test for /stats/investments"
```

---

## Task 23: Integration tests — dashboard + base_currency override

**Files:**
- Modify: `tests/api/stats.rs`

- [ ] **Step 1: Tests**

```rust
#[tokio::test]
async fn dashboard_returns_month_totals_and_top_categories() {
    let db = TestPostgres::new().await;
    let app = build_app(db.pool.clone()).await;
    let (token, user_id) = auth_token();

    // Create a category and a few expenses in current month
    let cat = uuid::Uuid::new_v4();
    sqlx::query!(
        "INSERT INTO categories (id, user_id, name, color, created_at)
         VALUES ($1, $2, 'Food', NULL, NOW())",
        cat, user_id,
    ).execute(&db.pool).await.unwrap();

    let now = chrono::Utc::now();
    seed_account_and_transaction(&db.pool, user_id, "UAH", rust_decimal::Decimal::new(2000, 0), "Income",  None,      now).await;
    seed_account_and_transaction(&db.pool, user_id, "UAH", rust_decimal::Decimal::new(800, 0),  "Expense", Some(cat), now).await;

    let response = app
        .get("/stats/dashboard")
        .add_header("authorization", format!("Bearer {token}"))
        .await;

    response.assert_status_ok();
    let body: serde_json::Value = response.json();
    assert_eq!(body["base_currency"], "UAH");
    assert_eq!(body["month_income"], "2000");
    assert_eq!(body["month_expense"], "800");
    let cats = body["top_categories"].as_array().unwrap();
    assert_eq!(cats[0]["name"], "Food");
}

#[tokio::test]
async fn base_currency_query_param_overrides_user_setting() {
    let db = TestPostgres::new().await;
    let date = chrono::NaiveDate::from_ymd_opt(2026, 5, 10).unwrap();
    seed_fx_rate(&db.pool, date, "USD", rust_decimal::Decimal::new(40, 0)).await;
    let app = build_app(db.pool.clone()).await;
    let (token, user_id) = auth_token();

    let when = chrono::Utc.with_ymd_and_hms(2026, 5, 10, 12, 0, 0).unwrap();
    seed_account_and_transaction(&db.pool, user_id, "UAH", rust_decimal::Decimal::new(4000, 0), "Expense", None, when).await;

    let response = app
        .get("/stats/categories?base_currency=USD&from=1746057600&to=1746489600")
        .add_header("authorization", format!("Bearer {token}"))
        .await;

    response.assert_status_ok();
    let body: serde_json::Value = response.json();
    assert_eq!(body["base_currency"], "USD");
    assert_eq!(body["items"][0]["total"], "100"); // 4000 UAH / 40 = 100 USD
}
```

- [ ] **Step 2: Run**

```bash
cargo test --test api stats:: 2>&1 | tail -50
```

Expected: all stats tests pass.

- [ ] **Step 3: Commit**

```bash
git add tests/api/stats.rs
git commit -m "test(api): dashboard + base_currency override integration tests"
```

---

## Task 24: OpenAPI spec update

**Files:**
- Modify: `static/openapi.json`

- [ ] **Step 1: Add the new paths and schemas**

For each new endpoint (`/stats/dashboard`, `/stats/balance-history`,
`/stats/cashflow`, `/stats/categories`, `/stats/investments`, `/me/settings`),
add a path entry mirroring the existing structure (auth via `bearer_auth`,
2xx response schema, 400 / 401).

Define new component schemas for `DashboardResponse`,
`BalanceHistoryResponse`, `CashflowResponse`, `CategoriesResponse`,
`InvestmentsResponse`, `UserSettingsResponse`,
`UpdateUserSettingsRequest`, `MissingRateDto`,
`CategoryBreakdownDto`, `BalanceHistoryPointDto`, `CashflowPointDto`,
`TickerHoldingDto`.

Keep field names exactly aligned with the DTOs in `src/api/dto.rs`.

- [ ] **Step 2: Verify the existing test that validates openapi.json (if any) still passes**

```bash
cargo test --test api -- --nocapture 2>&1 | tail -20
```

Expected: existing tests pass.

- [ ] **Step 3: Commit**

```bash
git add static/openapi.json
git commit -m "docs(openapi): add /stats and /me/settings paths and schemas"
```

---

## Task 25: Final verification

- [ ] **Step 1: Full test run**

```bash
cargo test 2>&1 | tail -50
```

Expected: all tests pass.

- [ ] **Step 2: Lint and format**

```bash
cargo clippy --all-targets -- -D warnings 2>&1 | tail -30
cargo fmt -- --check 2>&1 | tail -10
```

Expected: no warnings, no formatting changes needed. Run `cargo fmt` if
the check fails and commit any formatting fixes.

- [ ] **Step 3: Smoke check `cargo run`**

If a local Postgres + `.env` is available, briefly:

```bash
cargo run 2>&1 | head -20
```

Expected: server logs `listening on …` and `fetching JWKS from …`. Stop
with Ctrl-C. (Skip if env not configured.)

- [ ] **Step 4: Final commit if anything changed**

```bash
git status
git diff
# Commit only if there are pending fmt/clippy fixes:
git add -p
git commit -m "chore: final lint/fmt cleanup for stats feature"
```

---

## Self-Review

**Spec coverage check:**

| Spec section | Task |
|---|---|
| `/stats/dashboard` | Task 16 (repo) + 18 (handler) + 23 (test) |
| `/stats/balance-history` | Task 14 + 18 + 21 |
| `/stats/cashflow` | Task 13 + 18 + 21 |
| `/stats/categories` | Task 12 + 18 + 20 |
| `/stats/investments` | Task 15 (legs) + 17 (avg-cost) + 18 + 22 |
| `fx_rates` table | Task 1 |
| NBU source | Task 6 |
| `FxSyncUseCase` | Task 9 |
| Daily background sync | Task 19 |
| Rate-as-of-date lookup | Task 7 + tests in Task 8 |
| `user_settings` table | Task 2 |
| `GET /me/settings` | Task 11 |
| `PATCH /me/settings` | Task 11 |
| `partial` flag + `missing_rates` | Threaded through Tasks 12–18 |
| Cache-Control max-age=60 | Task 18 (`cache_headers`) |
| Average-cost realized P&L | Task 17 with unit tests |
| OpenAPI update | Task 24 |
| Investigation: Transfer sign | Task 0 |
| Validation (invalid base_currency, dates) | Task 11 (`set_base_currency`), Task 18 (`parse_range`/`parse_granularity`), Task 17 (`resolve_base_currency`) |
| Top-N categories on dashboard | Task 16 + Task 23 test |

**Placeholder scan:** the only intentional placeholders are inside Task 14
(balance-history) where the conversion `CASE` and Transfer rule are marked
`<...>` because they depend on Task 0's findings and would otherwise repeat
verbatim from Tasks 12 and 13. The pattern to copy is shown explicitly in
those earlier tasks.

**Type consistency:** `Granularity::as_sql()` returns `&'static str`, used
in handler responses (`granularity` field) and SQL formatting consistently.
`StatsRange` fields match across domain trait, service, and infrastructure.
`MissingRate { date: NaiveDate, currency: String }` matches in domain and
flows into `MissingRateDto { date: String, currency: String }` (formatted
`%Y-%m-%d`) at the API edge.

---

## Execution Handoff

Plan complete and saved to `docs/superpowers/plans/2026-05-11-stats-and-graphs.md`. Two execution options:

**1. Subagent-Driven (recommended)** — I dispatch a fresh subagent per task, review between tasks, fast iteration.

**2. Inline Execution** — Execute tasks in this session using executing-plans, batch execution with checkpoints.

Which approach?
