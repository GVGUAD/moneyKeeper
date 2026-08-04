# Subscriptions Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add user subscriptions — ingest receipts from Gmail, persist `Subscription` and `SubscriptionCharge` aggregates, match charges to monobank transactions, auto-categorize, and expose inventory + forecast + manual link API.

**Architecture:** New domain aggregates (`EmailConnection`, `Subscription`, `SubscriptionCharge`) with repository traits, mirroring the existing `BankConnection` pattern. Source-pluggable ingestion via an `EmailFetcher` trait (Gmail API impl in v1) and per-provider `ReceiptParser` impls (Google Play, Apple, Netflix). Application services orchestrate sync, matching (FX-converted amount ±5% / time ±3d), and lapse detection. Hourly tokio scheduler drives the sync loop, and `MonobankSyncUseCase` triggers the matcher at the end.

**Tech Stack:** Rust 2024, axum 0.8, sqlx 0.8 (Postgres), tokio, anyhow + thiserror, async-trait, uuid, chrono, rust_decimal. New crates: `oauth2`, `scraper`, `regex`. Postgres via testcontainers in tests (existing `test_db::fresh_pool`).

**Spec:** `docs/superpowers/specs/2026-05-26-subscriptions-design.md`

---

## Task 1: Migration — email_connections table

**Files:**
- Create: `src/infrastructure/migrations/0009_email_connections.sql`

- [ ] **Step 1: Write the migration**

```sql
CREATE TABLE email_connections (
    id UUID PRIMARY KEY,
    user_id UUID NOT NULL,
    provider TEXT NOT NULL,
    email_address TEXT NOT NULL,
    oauth_access_token TEXT NOT NULL,
    oauth_refresh_token TEXT NOT NULL,
    access_token_expires_at BIGINT NOT NULL,
    status TEXT NOT NULL,
    last_synced_at BIGINT,
    last_history_id TEXT,
    created_at BIGINT NOT NULL
);

CREATE INDEX email_connections_user_id_idx ON email_connections (user_id);
CREATE INDEX email_connections_status_idx ON email_connections (status);
```

(Timestamps are stored as unix seconds (`BIGINT`) to match existing tables — see `bank_connections` row mapping in `src/infrastructure/monobank_repository.rs:42-44`.)

- [ ] **Step 2: Verify migration compiles by building**

Run: `cargo build`
Expected: PASS (compile-time check via sqlx-migrate macro is exercised by `test_db::fresh_pool` only — but build must still succeed).

- [ ] **Step 3: Run an integration test that touches `fresh_pool`**

Run: `cargo test --test api -- helpers 2>&1 | tail -5` (any pre-existing test that boots the pool)
Expected: PASS — migration must succeed against a fresh container.

- [ ] **Step 4: Commit**

```bash
git add src/infrastructure/migrations/0009_email_connections.sql
git commit -m "feat: add email_connections table"
```

---

## Task 2: Migration — subscriptions table

**Files:**
- Create: `src/infrastructure/migrations/0010_subscriptions.sql`

- [ ] **Step 1: Write the migration**

```sql
CREATE TABLE subscriptions (
    id UUID PRIMARY KEY,
    user_id UUID NOT NULL,
    provider TEXT NOT NULL,
    product_name TEXT NOT NULL,
    merchant_key TEXT NOT NULL,
    amount NUMERIC NOT NULL,
    currency TEXT NOT NULL,
    billing_period TEXT NOT NULL,
    status TEXT NOT NULL,
    started_at BIGINT NOT NULL,
    last_charged_at BIGINT,
    next_expected_at BIGINT,
    category_id UUID,
    created_at BIGINT NOT NULL,

    CONSTRAINT subscriptions_user_merchant_unique UNIQUE (user_id, merchant_key)
);

CREATE INDEX subscriptions_user_status_idx ON subscriptions (user_id, status);
```

- [ ] **Step 2: Verify build + pool boot**

Run: `cargo test --test api -- helpers 2>&1 | tail -5`
Expected: PASS

- [ ] **Step 3: Commit**

```bash
git add src/infrastructure/migrations/0010_subscriptions.sql
git commit -m "feat: add subscriptions table"
```

---

## Task 3: Migration — subscription_charges table

**Files:**
- Create: `src/infrastructure/migrations/0011_subscription_charges.sql`

- [ ] **Step 1: Write the migration**

```sql
CREATE TABLE subscription_charges (
    id UUID PRIMARY KEY,
    subscription_id UUID NOT NULL REFERENCES subscriptions(id) ON DELETE CASCADE,
    user_id UUID NOT NULL,
    amount NUMERIC NOT NULL,
    currency TEXT NOT NULL,
    charged_at BIGINT NOT NULL,
    email_message_id TEXT NOT NULL,
    kind TEXT NOT NULL,
    transaction_id UUID REFERENCES transactions(id) ON DELETE SET NULL,
    match_status TEXT NOT NULL,
    created_at BIGINT NOT NULL,

    CONSTRAINT subscription_charges_message_id_unique UNIQUE (email_message_id)
);

CREATE INDEX subscription_charges_user_charged_idx ON subscription_charges (user_id, charged_at);
CREATE INDEX subscription_charges_tx_idx ON subscription_charges (transaction_id);
CREATE INDEX subscription_charges_pending_idx
    ON subscription_charges (user_id, match_status)
    WHERE match_status IN ('Pending', 'Unmatched');
```

- [ ] **Step 2: Verify build + pool boot**

Run: `cargo test --test api -- helpers 2>&1 | tail -5`
Expected: PASS

- [ ] **Step 3: Commit**

```bash
git add src/infrastructure/migrations/0011_subscription_charges.sql
git commit -m "feat: add subscription_charges table"
```

---

## Task 4: Add new dependencies

**Files:**
- Modify: `Cargo.toml`

- [ ] **Step 1: Add crates**

Edit `[dependencies]`:

```toml
oauth2 = "5"
scraper = "0.20"
regex = "1"
```

- [ ] **Step 2: Verify**

Run: `cargo build`
Expected: PASS — pulls and compiles the new crates.

- [ ] **Step 3: Commit**

```bash
git add Cargo.toml Cargo.lock
git commit -m "deps: add oauth2, scraper, regex"
```

---

## Task 5: Domain — EmailConnection aggregate + repo trait

**Files:**
- Create: `src/domain/email_connection.rs`
- Modify: `src/domain/mod.rs`

- [ ] **Step 1: Write failing test for enum roundtrip**

Create `src/domain/email_connection.rs`:

```rust
use chrono::{DateTime, Utc};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq)]
pub enum EmailProvider {
    Gmail,
}

impl EmailProvider {
    pub fn as_str(&self) -> &'static str {
        match self {
            EmailProvider::Gmail => "gmail",
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> anyhow::Result<Self> {
        match s {
            "gmail" => Ok(EmailProvider::Gmail),
            other => Err(anyhow::anyhow!("unknown email provider: {other}")),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum EmailConnectionStatus {
    Pending,
    Connected,
    Failed,
}

impl EmailConnectionStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Connected => "connected",
            Self::Failed => "failed",
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> anyhow::Result<Self> {
        match s {
            "pending" => Ok(Self::Pending),
            "connected" => Ok(Self::Connected),
            "failed" => Ok(Self::Failed),
            other => Err(anyhow::anyhow!("unknown email connection status: {other}")),
        }
    }
}

#[derive(Debug, Clone)]
pub struct EmailConnection {
    pub id: Uuid,
    pub user_id: Uuid,
    pub provider: EmailProvider,
    pub email_address: String,
    pub oauth_access_token: String,
    pub oauth_refresh_token: String,
    pub access_token_expires_at: DateTime<Utc>,
    pub status: EmailConnectionStatus,
    pub last_synced_at: Option<DateTime<Utc>>,
    pub last_history_id: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[async_trait::async_trait]
pub trait EmailConnectionRepository: Send + Sync {
    async fn create(&self, conn: &EmailConnection) -> anyhow::Result<()>;
    async fn find_by_id(&self, id: Uuid, user_id: Uuid) -> anyhow::Result<Option<EmailConnection>>;
    async fn list_by_user(&self, user_id: Uuid) -> anyhow::Result<Vec<EmailConnection>>;
    async fn list_connected(&self) -> anyhow::Result<Vec<EmailConnection>>;
    async fn update_tokens(
        &self,
        id: Uuid,
        access_token: &str,
        refresh_token: &str,
        expires_at: DateTime<Utc>,
    ) -> anyhow::Result<()>;
    async fn update_status(&self, id: Uuid, status: EmailConnectionStatus) -> anyhow::Result<()>;
    async fn update_sync_cursor(
        &self,
        id: Uuid,
        last_synced_at: DateTime<Utc>,
        last_history_id: Option<String>,
    ) -> anyhow::Result<()>;
    async fn delete(&self, id: Uuid, user_id: Uuid) -> anyhow::Result<()>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn email_provider_roundtrip() {
        for p in [EmailProvider::Gmail] {
            assert_eq!(EmailProvider::from_str(p.as_str()).unwrap(), p);
        }
    }

    #[test]
    fn status_roundtrip() {
        for s in [
            EmailConnectionStatus::Pending,
            EmailConnectionStatus::Connected,
            EmailConnectionStatus::Failed,
        ] {
            assert_eq!(EmailConnectionStatus::from_str(s.as_str()).unwrap(), s);
        }
    }

    #[test]
    fn unknown_provider_errors() {
        assert!(EmailProvider::from_str("yahoo").is_err());
    }
}
```

- [ ] **Step 2: Register module**

Edit `src/domain/mod.rs` — add `pub mod email_connection;` alongside existing module declarations.

- [ ] **Step 3: Run tests**

Run: `cargo test domain::email_connection`
Expected: 3 tests PASS.

- [ ] **Step 4: Commit**

```bash
git add src/domain/email_connection.rs src/domain/mod.rs
git commit -m "feat(domain): EmailConnection aggregate and repository trait"
```

---

## Task 6: Domain — Subscription aggregate

**Files:**
- Create: `src/domain/subscription.rs`
- Modify: `src/domain/mod.rs`

- [ ] **Step 1: Write the file with enums, struct, repo trait, and unit tests**

```rust
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq)]
pub enum SubscriptionProvider {
    GooglePlay,
    AppleAppStore,
    Netflix,
    Other,
}

impl SubscriptionProvider {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::GooglePlay => "google_play",
            Self::AppleAppStore => "apple_app_store",
            Self::Netflix => "netflix",
            Self::Other => "other",
        }
    }
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> anyhow::Result<Self> {
        match s {
            "google_play" => Ok(Self::GooglePlay),
            "apple_app_store" => Ok(Self::AppleAppStore),
            "netflix" => Ok(Self::Netflix),
            "other" => Ok(Self::Other),
            other => Err(anyhow::anyhow!("unknown subscription provider: {other}")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BillingPeriod {
    Weekly,
    Monthly,
    Yearly,
}

impl BillingPeriod {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Weekly => "weekly",
            Self::Monthly => "monthly",
            Self::Yearly => "yearly",
        }
    }
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> anyhow::Result<Self> {
        match s {
            "weekly" => Ok(Self::Weekly),
            "monthly" => Ok(Self::Monthly),
            "yearly" => Ok(Self::Yearly),
            other => Err(anyhow::anyhow!("unknown billing period: {other}")),
        }
    }
    /// Days in one billing cycle, used for forecast normalization.
    pub fn cycle_days(&self) -> i64 {
        match self {
            Self::Weekly => 7,
            Self::Monthly => 30,
            Self::Yearly => 365,
        }
    }
    /// Returns `from + one cycle`.
    pub fn next_after(&self, from: DateTime<Utc>) -> DateTime<Utc> {
        from + chrono::Duration::days(self.cycle_days())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum SubscriptionStatus {
    Active,
    Inactive,
}

impl SubscriptionStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Inactive => "inactive",
        }
    }
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> anyhow::Result<Self> {
        match s {
            "active" => Ok(Self::Active),
            "inactive" => Ok(Self::Inactive),
            other => Err(anyhow::anyhow!("unknown subscription status: {other}")),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Subscription {
    pub id: Uuid,
    pub user_id: Uuid,
    pub provider: SubscriptionProvider,
    pub product_name: String,
    pub merchant_key: String,
    pub amount: Decimal,
    pub currency: String,
    pub billing_period: BillingPeriod,
    pub status: SubscriptionStatus,
    pub started_at: DateTime<Utc>,
    pub last_charged_at: Option<DateTime<Utc>>,
    pub next_expected_at: Option<DateTime<Utc>>,
    pub category_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Default)]
pub struct SubscriptionListFilter {
    pub status: Option<SubscriptionStatus>,
}

#[async_trait::async_trait]
pub trait SubscriptionRepository: Send + Sync {
    async fn upsert_by_merchant_key(&self, sub: &Subscription) -> anyhow::Result<Subscription>;
    async fn find_by_id(&self, id: Uuid, user_id: Uuid) -> anyhow::Result<Option<Subscription>>;
    async fn list_by_user(
        &self,
        user_id: Uuid,
        filter: &SubscriptionListFilter,
    ) -> anyhow::Result<Vec<Subscription>>;
    async fn update_after_charge(
        &self,
        id: Uuid,
        last_charged_at: DateTime<Utc>,
        next_expected_at: DateTime<Utc>,
        status: SubscriptionStatus,
    ) -> anyhow::Result<()>;
    async fn update_editable_fields(
        &self,
        id: Uuid,
        user_id: Uuid,
        product_name: Option<String>,
        category_id: Option<Option<Uuid>>,
        billing_period: Option<BillingPeriod>,
        status: Option<SubscriptionStatus>,
    ) -> anyhow::Result<()>;
    async fn list_lapsed(&self, before: DateTime<Utc>) -> anyhow::Result<Vec<Subscription>>;
    async fn delete(&self, id: Uuid, user_id: Uuid) -> anyhow::Result<()>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_roundtrip() {
        for p in [
            SubscriptionProvider::GooglePlay,
            SubscriptionProvider::AppleAppStore,
            SubscriptionProvider::Netflix,
            SubscriptionProvider::Other,
        ] {
            assert_eq!(SubscriptionProvider::from_str(p.as_str()).unwrap(), p);
        }
    }

    #[test]
    fn billing_period_cycle_days() {
        assert_eq!(BillingPeriod::Weekly.cycle_days(), 7);
        assert_eq!(BillingPeriod::Monthly.cycle_days(), 30);
        assert_eq!(BillingPeriod::Yearly.cycle_days(), 365);
    }

    #[test]
    fn billing_period_next_after_is_one_cycle_later() {
        let from = DateTime::<Utc>::from_timestamp(1_700_000_000, 0).unwrap();
        let next = BillingPeriod::Monthly.next_after(from);
        assert_eq!((next - from).num_days(), 30);
    }
}
```

- [ ] **Step 2: Register module**

Edit `src/domain/mod.rs` — add `pub mod subscription;`.

- [ ] **Step 3: Run tests**

Run: `cargo test domain::subscription`
Expected: 3 tests PASS.

- [ ] **Step 4: Commit**

```bash
git add src/domain/subscription.rs src/domain/mod.rs
git commit -m "feat(domain): Subscription aggregate and repository trait"
```

---

## Task 7: Domain — SubscriptionCharge aggregate

**Files:**
- Create: `src/domain/subscription_charge.rs`
- Modify: `src/domain/mod.rs`

- [ ] **Step 1: Write the file**

```rust
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq)]
pub enum ChargeMatchStatus {
    Pending,
    Matched,
    Unmatched,
}

impl ChargeMatchStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "Pending",
            Self::Matched => "Matched",
            Self::Unmatched => "Unmatched",
        }
    }
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> anyhow::Result<Self> {
        match s {
            "Pending" => Ok(Self::Pending),
            "Matched" => Ok(Self::Matched),
            "Unmatched" => Ok(Self::Unmatched),
            other => Err(anyhow::anyhow!("unknown charge match status: {other}")),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ReceiptKind {
    NewSubscription,
    Renewal,
    OneTimePurchase,
    Refund,
}

impl ReceiptKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::NewSubscription => "new_subscription",
            Self::Renewal => "renewal",
            Self::OneTimePurchase => "one_time_purchase",
            Self::Refund => "refund",
        }
    }
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> anyhow::Result<Self> {
        match s {
            "new_subscription" => Ok(Self::NewSubscription),
            "renewal" => Ok(Self::Renewal),
            "one_time_purchase" => Ok(Self::OneTimePurchase),
            "refund" => Ok(Self::Refund),
            other => Err(anyhow::anyhow!("unknown receipt kind: {other}")),
        }
    }
}

#[derive(Debug, Clone)]
pub struct SubscriptionCharge {
    pub id: Uuid,
    pub subscription_id: Uuid,
    pub user_id: Uuid,
    pub amount: Decimal,
    pub currency: String,
    pub charged_at: DateTime<Utc>,
    pub email_message_id: String,
    pub kind: ReceiptKind,
    pub transaction_id: Option<Uuid>,
    pub match_status: ChargeMatchStatus,
    pub created_at: DateTime<Utc>,
}

#[async_trait::async_trait]
pub trait SubscriptionChargeRepository: Send + Sync {
    /// INSERT ... ON CONFLICT (email_message_id) DO NOTHING.
    /// Returns the persisted (or pre-existing) charge id and a boolean indicating insertion.
    async fn create_idempotent(
        &self,
        charge: &SubscriptionCharge,
    ) -> anyhow::Result<(Uuid, bool)>;
    async fn find_by_id(
        &self,
        id: Uuid,
        user_id: Uuid,
    ) -> anyhow::Result<Option<SubscriptionCharge>>;
    async fn list_pending_for_user(&self, user_id: Uuid)
        -> anyhow::Result<Vec<SubscriptionCharge>>;
    async fn list_for_subscription(
        &self,
        subscription_id: Uuid,
        user_id: Uuid,
    ) -> anyhow::Result<Vec<SubscriptionCharge>>;
    async fn update_match(
        &self,
        id: Uuid,
        transaction_id: Option<Uuid>,
        match_status: ChargeMatchStatus,
    ) -> anyhow::Result<()>;
    async fn mark_pending_older_than_unmatched(
        &self,
        threshold: DateTime<Utc>,
    ) -> anyhow::Result<u64>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn match_status_roundtrip() {
        for s in [
            ChargeMatchStatus::Pending,
            ChargeMatchStatus::Matched,
            ChargeMatchStatus::Unmatched,
        ] {
            assert_eq!(ChargeMatchStatus::from_str(s.as_str()).unwrap(), s);
        }
    }

    #[test]
    fn receipt_kind_roundtrip() {
        for k in [
            ReceiptKind::NewSubscription,
            ReceiptKind::Renewal,
            ReceiptKind::OneTimePurchase,
            ReceiptKind::Refund,
        ] {
            assert_eq!(ReceiptKind::from_str(k.as_str()).unwrap(), k);
        }
    }
}
```

- [ ] **Step 2: Register module**

Edit `src/domain/mod.rs` — add `pub mod subscription_charge;`.

- [ ] **Step 3: Run tests**

Run: `cargo test domain::subscription_charge`
Expected: 2 tests PASS.

- [ ] **Step 4: Commit**

```bash
git add src/domain/subscription_charge.rs src/domain/mod.rs
git commit -m "feat(domain): SubscriptionCharge aggregate and repository trait"
```

---

## Task 8: Domain — RawEmail + EmailFetcher trait

**Files:**
- Create: `src/domain/email.rs`
- Modify: `src/domain/mod.rs`

- [ ] **Step 1: Write the trait**

```rust
use chrono::{DateTime, Utc};

use crate::domain::email_connection::EmailConnection;

#[derive(Debug, Clone)]
pub struct RawEmail {
    pub message_id: String,
    pub from: String,
    pub subject: String,
    pub received_at: DateTime<Utc>,
    pub body_text: Option<String>,
    pub body_html: Option<String>,
}

#[async_trait::async_trait]
pub trait EmailFetcher: Send + Sync {
    /// Fetch new emails since the connection's cursor. Returns the list of new
    /// emails (in arbitrary order) plus the cursor value to persist back on the
    /// connection (`last_history_id`).
    async fn fetch_new(
        &self,
        conn: &EmailConnection,
    ) -> anyhow::Result<(Vec<RawEmail>, Option<String>)>;
}
```

- [ ] **Step 2: Register module**

Edit `src/domain/mod.rs` — add `pub mod email;`.

- [ ] **Step 3: Verify build**

Run: `cargo build`
Expected: PASS

- [ ] **Step 4: Commit**

```bash
git add src/domain/email.rs src/domain/mod.rs
git commit -m "feat(domain): RawEmail and EmailFetcher trait"
```

---

## Task 9: Domain — ParsedReceipt + ReceiptParser trait

**Files:**
- Create: `src/domain/receipt_parser.rs`
- Modify: `src/domain/mod.rs`

- [ ] **Step 1: Write the trait**

```rust
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;

use crate::domain::email::RawEmail;
use crate::domain::subscription::{BillingPeriod, SubscriptionProvider};
use crate::domain::subscription_charge::ReceiptKind;

#[derive(Debug, Clone)]
pub struct ParsedReceipt {
    pub provider: SubscriptionProvider,
    pub product_name: String,
    pub merchant_key: String,
    pub amount: Decimal,
    pub currency: String,
    pub charged_at: DateTime<Utc>,
    pub billing_period_hint: Option<BillingPeriod>,
    pub kind: ReceiptKind,
}

pub trait ReceiptParser: Send + Sync {
    fn matches_sender(&self, from: &str) -> bool;
    fn parse(&self, email: &RawEmail) -> anyhow::Result<Option<ParsedReceipt>>;
}
```

- [ ] **Step 2: Register module**

Edit `src/domain/mod.rs` — add `pub mod receipt_parser;`.

- [ ] **Step 3: Verify build**

Run: `cargo build`
Expected: PASS

- [ ] **Step 4: Commit**

```bash
git add src/domain/receipt_parser.rs src/domain/mod.rs
git commit -m "feat(domain): ParsedReceipt and ReceiptParser trait"
```

---

## Task 10: Domain — SubscriptionError

**Files:**
- Create: `src/domain/subscription_error.rs`
- Modify: `src/domain/mod.rs`

- [ ] **Step 1: Write the error enum**

```rust
#[derive(Debug, thiserror::Error)]
pub enum SubscriptionError {
    #[error("no parser registered for sender: {0}")]
    ParserNotFound(String),
    #[error("OAuth refresh failed: {0}")]
    OAuthRefreshFailed(String),
    #[error("duplicate charge for message id: {0}")]
    DuplicateCharge(String),
    #[error("ambiguous match for charge {charge_id}: {candidate_count} candidates")]
    MatchAmbiguous { charge_id: uuid::Uuid, candidate_count: usize },
    #[error("email connection not found")]
    ConnectionNotFound,
    #[error("subscription not found")]
    SubscriptionNotFound,
    #[error("subscription charge not found")]
    ChargeNotFound,
}
```

- [ ] **Step 2: Register module**

Edit `src/domain/mod.rs` — add `pub mod subscription_error;`.

- [ ] **Step 3: Verify build**

Run: `cargo build`
Expected: PASS

- [ ] **Step 4: Commit**

```bash
git add src/domain/subscription_error.rs src/domain/mod.rs
git commit -m "feat(domain): SubscriptionError enum"
```

---

## Task 11: Infra — PgEmailConnectionRepository

**Files:**
- Create: `src/infrastructure/email_connection_repository.rs`
- Modify: `src/infrastructure/mod.rs`

This task mirrors `src/infrastructure/monobank_repository.rs` (see `PgBankConnectionRepository`). Same row→struct pattern, same i64-timestamp conventions.

- [ ] **Step 1: Write integration tests first**

Add the test module at the bottom of the new file:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::infrastructure::test_db;

    fn sample_conn(user_id: Uuid) -> EmailConnection {
        EmailConnection {
            id: Uuid::new_v4(),
            user_id,
            provider: EmailProvider::Gmail,
            email_address: "alice@example.com".to_string(),
            oauth_access_token: "access-1".to_string(),
            oauth_refresh_token: "refresh-1".to_string(),
            access_token_expires_at: Utc::now() + chrono::Duration::hours(1),
            status: EmailConnectionStatus::Pending,
            last_synced_at: None,
            last_history_id: None,
            created_at: Utc::now(),
        }
    }

    #[tokio::test]
    async fn create_and_find_by_id() {
        let pool = test_db::fresh_pool().await;
        let repo = PgEmailConnectionRepository::new(pool);
        let user_id = Uuid::new_v4();
        let conn = sample_conn(user_id);
        let id = conn.id;
        repo.create(&conn).await.unwrap();
        let found = repo.find_by_id(id, user_id).await.unwrap().unwrap();
        assert_eq!(found.email_address, "alice@example.com");
        assert_eq!(found.status, EmailConnectionStatus::Pending);
    }

    #[tokio::test]
    async fn update_tokens_persists() {
        let pool = test_db::fresh_pool().await;
        let repo = PgEmailConnectionRepository::new(pool);
        let user_id = Uuid::new_v4();
        let conn = sample_conn(user_id);
        let id = conn.id;
        repo.create(&conn).await.unwrap();
        let new_exp = Utc::now() + chrono::Duration::hours(2);
        repo.update_tokens(id, "new-access", "new-refresh", new_exp)
            .await
            .unwrap();
        let found = repo.find_by_id(id, user_id).await.unwrap().unwrap();
        assert_eq!(found.oauth_access_token, "new-access");
        assert_eq!(found.oauth_refresh_token, "new-refresh");
        assert_eq!(found.access_token_expires_at.timestamp(), new_exp.timestamp());
    }

    #[tokio::test]
    async fn list_connected_filters_by_status() {
        let pool = test_db::fresh_pool().await;
        let repo = PgEmailConnectionRepository::new(pool);
        let user_id = Uuid::new_v4();
        let pending = sample_conn(user_id);
        let connected = sample_conn(user_id);
        repo.create(&pending).await.unwrap();
        repo.create(&connected).await.unwrap();
        repo.update_status(connected.id, EmailConnectionStatus::Connected)
            .await
            .unwrap();
        let all = repo.list_connected().await.unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].id, connected.id);
    }

    #[tokio::test]
    async fn update_sync_cursor_persists() {
        let pool = test_db::fresh_pool().await;
        let repo = PgEmailConnectionRepository::new(pool);
        let user_id = Uuid::new_v4();
        let conn = sample_conn(user_id);
        let id = conn.id;
        repo.create(&conn).await.unwrap();
        let now = Utc::now();
        repo.update_sync_cursor(id, now, Some("hist-42".to_string()))
            .await
            .unwrap();
        let found = repo.find_by_id(id, user_id).await.unwrap().unwrap();
        assert_eq!(found.last_history_id.as_deref(), Some("hist-42"));
        assert_eq!(found.last_synced_at.unwrap().timestamp(), now.timestamp());
    }
}
```

- [ ] **Step 2: Write the implementation**

```rust
use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use crate::domain::email_connection::{
    EmailConnection, EmailConnectionRepository, EmailConnectionStatus, EmailProvider,
};

pub struct PgEmailConnectionRepository {
    pool: PgPool,
}

impl PgEmailConnectionRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[derive(sqlx::FromRow)]
struct Row {
    id: Uuid,
    user_id: Uuid,
    provider: String,
    email_address: String,
    oauth_access_token: String,
    oauth_refresh_token: String,
    access_token_expires_at: i64,
    status: String,
    last_synced_at: Option<i64>,
    last_history_id: Option<String>,
    created_at: i64,
}

fn row_to_conn(r: Row) -> anyhow::Result<EmailConnection> {
    Ok(EmailConnection {
        id: r.id,
        user_id: r.user_id,
        provider: EmailProvider::from_str(&r.provider)?,
        email_address: r.email_address,
        oauth_access_token: r.oauth_access_token,
        oauth_refresh_token: r.oauth_refresh_token,
        access_token_expires_at: DateTime::from_timestamp(r.access_token_expires_at, 0)
            .ok_or_else(|| anyhow::anyhow!("invalid access_token_expires_at"))?,
        status: EmailConnectionStatus::from_str(&r.status)?,
        last_synced_at: r.last_synced_at.and_then(|t| DateTime::from_timestamp(t, 0)),
        last_history_id: r.last_history_id,
        created_at: DateTime::from_timestamp(r.created_at, 0)
            .ok_or_else(|| anyhow::anyhow!("invalid created_at"))?,
    })
}

#[async_trait::async_trait]
impl EmailConnectionRepository for PgEmailConnectionRepository {
    async fn create(&self, conn: &EmailConnection) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO email_connections \
             (id, user_id, provider, email_address, oauth_access_token, oauth_refresh_token, \
              access_token_expires_at, status, last_synced_at, last_history_id, created_at) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)",
        )
        .bind(conn.id)
        .bind(conn.user_id)
        .bind(conn.provider.as_str())
        .bind(&conn.email_address)
        .bind(&conn.oauth_access_token)
        .bind(&conn.oauth_refresh_token)
        .bind(conn.access_token_expires_at.timestamp())
        .bind(conn.status.as_str())
        .bind(conn.last_synced_at.map(|d| d.timestamp()))
        .bind(&conn.last_history_id)
        .bind(conn.created_at.timestamp())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn find_by_id(&self, id: Uuid, user_id: Uuid) -> anyhow::Result<Option<EmailConnection>> {
        let row = sqlx::query_as::<_, Row>(
            "SELECT * FROM email_connections WHERE id=$1 AND user_id=$2",
        )
        .bind(id)
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(row_to_conn).transpose()
    }

    async fn list_by_user(&self, user_id: Uuid) -> anyhow::Result<Vec<EmailConnection>> {
        let rows = sqlx::query_as::<_, Row>(
            "SELECT * FROM email_connections WHERE user_id=$1 ORDER BY created_at",
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(row_to_conn).collect()
    }

    async fn list_connected(&self) -> anyhow::Result<Vec<EmailConnection>> {
        let rows = sqlx::query_as::<_, Row>(
            "SELECT * FROM email_connections WHERE status='connected' ORDER BY created_at",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(row_to_conn).collect()
    }

    async fn update_tokens(
        &self,
        id: Uuid,
        access_token: &str,
        refresh_token: &str,
        expires_at: DateTime<Utc>,
    ) -> anyhow::Result<()> {
        sqlx::query(
            "UPDATE email_connections SET oauth_access_token=$1, oauth_refresh_token=$2, \
             access_token_expires_at=$3 WHERE id=$4",
        )
        .bind(access_token)
        .bind(refresh_token)
        .bind(expires_at.timestamp())
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn update_status(&self, id: Uuid, status: EmailConnectionStatus) -> anyhow::Result<()> {
        sqlx::query("UPDATE email_connections SET status=$1 WHERE id=$2")
            .bind(status.as_str())
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn update_sync_cursor(
        &self,
        id: Uuid,
        last_synced_at: DateTime<Utc>,
        last_history_id: Option<String>,
    ) -> anyhow::Result<()> {
        sqlx::query(
            "UPDATE email_connections SET last_synced_at=$1, last_history_id=$2 WHERE id=$3",
        )
        .bind(last_synced_at.timestamp())
        .bind(last_history_id)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn delete(&self, id: Uuid, user_id: Uuid) -> anyhow::Result<()> {
        sqlx::query("DELETE FROM email_connections WHERE id=$1 AND user_id=$2")
            .bind(id)
            .bind(user_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}
```

- [ ] **Step 3: Register module**

Edit `src/infrastructure/mod.rs` — add `pub mod email_connection_repository;`.

- [ ] **Step 4: Run tests**

Run: `cargo test infrastructure::email_connection_repository`
Expected: 4 tests PASS.

- [ ] **Step 5: Commit**

```bash
git add src/infrastructure/email_connection_repository.rs src/infrastructure/mod.rs
git commit -m "feat(infra): PgEmailConnectionRepository"
```

---

## Task 12: Infra — PgSubscriptionRepository

**Files:**
- Create: `src/infrastructure/subscription_repository.rs`
- Modify: `src/infrastructure/mod.rs`

- [ ] **Step 1: Write the test module**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::infrastructure::test_db;
    use rust_decimal_macros::dec;

    fn sample(user_id: Uuid, key: &str) -> Subscription {
        Subscription {
            id: Uuid::new_v4(),
            user_id,
            provider: SubscriptionProvider::Netflix,
            product_name: "Netflix Premium".to_string(),
            merchant_key: key.to_string(),
            amount: dec!(15.99),
            currency: "USD".to_string(),
            billing_period: BillingPeriod::Monthly,
            status: SubscriptionStatus::Active,
            started_at: Utc::now(),
            last_charged_at: None,
            next_expected_at: None,
            category_id: None,
            created_at: Utc::now(),
        }
    }

    #[tokio::test]
    async fn upsert_inserts_then_updates() {
        let pool = test_db::fresh_pool().await;
        let repo = PgSubscriptionRepository::new(pool);
        let user_id = Uuid::new_v4();
        let s1 = sample(user_id, "netflix.com:premium");
        let inserted = repo.upsert_by_merchant_key(&s1).await.unwrap();
        assert_eq!(inserted.id, s1.id);

        let mut s2 = sample(user_id, "netflix.com:premium");
        s2.amount = dec!(17.99);
        let upserted = repo.upsert_by_merchant_key(&s2).await.unwrap();
        assert_eq!(upserted.id, s1.id, "same id reused");
        assert_eq!(upserted.amount, dec!(17.99));
    }

    #[tokio::test]
    async fn list_lapsed_returns_active_past_threshold() {
        let pool = test_db::fresh_pool().await;
        let repo = PgSubscriptionRepository::new(pool);
        let user_id = Uuid::new_v4();
        let mut s = sample(user_id, "netflix.com:premium");
        s.next_expected_at = Some(Utc::now() - chrono::Duration::days(10));
        repo.upsert_by_merchant_key(&s).await.unwrap();
        let lapsed = repo
            .list_lapsed(Utc::now() - chrono::Duration::days(7))
            .await
            .unwrap();
        assert_eq!(lapsed.len(), 1);
    }
}
```

(Add `rust_decimal_macros = "1"` to `[dev-dependencies]` in `Cargo.toml` for `dec!`.)

- [ ] **Step 2: Write the implementation**

Standard sqlx pattern. Key behaviors:

- `upsert_by_merchant_key` uses `INSERT ... ON CONFLICT (user_id, merchant_key) DO UPDATE SET amount=$X, currency=$X, billing_period=$X, product_name=$X RETURNING *`. The row's pre-existing `id`, `started_at`, `category_id`, and `status` are preserved across upsert (do NOT overwrite them).
- `list_lapsed(before)` returns subscriptions where `status='active' AND next_expected_at IS NOT NULL AND next_expected_at < before` (timestamp seconds).
- `update_after_charge` writes `last_charged_at`, `next_expected_at`, `status` in a single UPDATE.
- `update_editable_fields` builds a partial UPDATE — use a `COALESCE`-style query with `$N IS NULL OR` guards, or fall back to constructing the SQL based on which `Option`s are `Some`. Both are acceptable; the COALESCE pattern is simpler:

```rust
sqlx::query(
    "UPDATE subscriptions SET \
       product_name   = COALESCE($1, product_name), \
       billing_period = COALESCE($2, billing_period), \
       status         = COALESCE($3, status), \
       category_id    = CASE WHEN $4::boolean THEN $5 ELSE category_id END \
     WHERE id=$6 AND user_id=$7",
)
.bind(product_name)
.bind(billing_period.map(|b| b.as_str()))
.bind(status.map(|s| s.as_str()))
.bind(category_id.is_some()) // explicit-set marker
.bind(category_id.flatten()) // value (or NULL if user passed null)
.bind(id)
.bind(user_id)
```

Implement `find_by_id`, `list_by_user`, `delete` like the `EmailConnection` repo.

- [ ] **Step 3: Register module**

Edit `src/infrastructure/mod.rs` — add `pub mod subscription_repository;`.

- [ ] **Step 4: Run tests**

Run: `cargo test infrastructure::subscription_repository`
Expected: 2 tests PASS.

- [ ] **Step 5: Commit**

```bash
git add src/infrastructure/subscription_repository.rs src/infrastructure/mod.rs Cargo.toml Cargo.lock
git commit -m "feat(infra): PgSubscriptionRepository"
```

---

## Task 13: Infra — PgSubscriptionChargeRepository

**Files:**
- Create: `src/infrastructure/subscription_charge_repository.rs`
- Modify: `src/infrastructure/mod.rs`

- [ ] **Step 1: Write the test module**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::subscription::{
        BillingPeriod, Subscription, SubscriptionProvider, SubscriptionStatus,
    };
    use crate::infrastructure::subscription_repository::PgSubscriptionRepository;
    use crate::infrastructure::test_db;
    use crate::domain::subscription::SubscriptionRepository;
    use rust_decimal_macros::dec;

    async fn make_subscription(pool: &sqlx::PgPool, user_id: Uuid) -> Uuid {
        let sub = Subscription {
            id: Uuid::new_v4(),
            user_id,
            provider: SubscriptionProvider::Netflix,
            product_name: "Netflix".to_string(),
            merchant_key: "netflix.com:premium".to_string(),
            amount: dec!(15.99),
            currency: "USD".to_string(),
            billing_period: BillingPeriod::Monthly,
            status: SubscriptionStatus::Active,
            started_at: Utc::now(),
            last_charged_at: None,
            next_expected_at: None,
            category_id: None,
            created_at: Utc::now(),
        };
        let id = sub.id;
        PgSubscriptionRepository::new(pool.clone())
            .upsert_by_merchant_key(&sub)
            .await
            .unwrap();
        id
    }

    fn charge(user_id: Uuid, sub_id: Uuid, msg: &str) -> SubscriptionCharge {
        SubscriptionCharge {
            id: Uuid::new_v4(),
            subscription_id: sub_id,
            user_id,
            amount: dec!(15.99),
            currency: "USD".to_string(),
            charged_at: Utc::now(),
            email_message_id: msg.to_string(),
            kind: ReceiptKind::Renewal,
            transaction_id: None,
            match_status: ChargeMatchStatus::Pending,
            created_at: Utc::now(),
        }
    }

    #[tokio::test]
    async fn create_idempotent_second_call_returns_false() {
        let pool = test_db::fresh_pool().await;
        let user_id = Uuid::new_v4();
        let sub_id = make_subscription(&pool, user_id).await;
        let repo = PgSubscriptionChargeRepository::new(pool);
        let c1 = charge(user_id, sub_id, "msg-1");
        let (id1, inserted1) = repo.create_idempotent(&c1).await.unwrap();
        assert!(inserted1);
        let c2 = charge(user_id, sub_id, "msg-1");
        let (id2, inserted2) = repo.create_idempotent(&c2).await.unwrap();
        assert!(!inserted2);
        assert_eq!(id1, id2);
    }

    #[tokio::test]
    async fn list_pending_for_user_filters_by_status() {
        let pool = test_db::fresh_pool().await;
        let user_id = Uuid::new_v4();
        let sub_id = make_subscription(&pool, user_id).await;
        let repo = PgSubscriptionChargeRepository::new(pool);
        let c1 = charge(user_id, sub_id, "msg-1");
        let c2 = charge(user_id, sub_id, "msg-2");
        repo.create_idempotent(&c1).await.unwrap();
        repo.create_idempotent(&c2).await.unwrap();
        repo.update_match(c2.id, None, ChargeMatchStatus::Unmatched)
            .await
            .unwrap();
        let pending = repo.list_pending_for_user(user_id).await.unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, c1.id);
    }
}
```

- [ ] **Step 2: Write the implementation**

Same shape as the other repos. Highlights:

- `create_idempotent`: `INSERT ... ON CONFLICT (email_message_id) DO NOTHING RETURNING id` — if `RETURNING` yields a row, return `(returned_id, true)`. If not, run a `SELECT id FROM subscription_charges WHERE email_message_id=$1` and return `(existing_id, false)`.
- `list_pending_for_user`: `WHERE user_id=$1 AND match_status='Pending'` ordered by `charged_at`.
- `mark_pending_older_than_unmatched(threshold)`: `UPDATE ... SET match_status='Unmatched' WHERE match_status='Pending' AND charged_at < $1` and return `rows_affected()`.
- `update_match`: `UPDATE ... SET transaction_id=$1, match_status=$2 WHERE id=$3`.

- [ ] **Step 3: Register module**

Edit `src/infrastructure/mod.rs` — add `pub mod subscription_charge_repository;`.

- [ ] **Step 4: Run tests**

Run: `cargo test infrastructure::subscription_charge_repository`
Expected: 2 tests PASS.

- [ ] **Step 5: Commit**

```bash
git add src/infrastructure/subscription_charge_repository.rs src/infrastructure/mod.rs
git commit -m "feat(infra): PgSubscriptionChargeRepository"
```

---

## Task 14: Parser — NetflixParser

**Files:**
- Create: `src/infrastructure/email/mod.rs`
- Create: `src/infrastructure/email/parsers/mod.rs`
- Create: `src/infrastructure/email/parsers/netflix.rs`
- Create: `tests/fixtures/receipts/netflix/renewal.txt`
- Modify: `src/infrastructure/mod.rs`

- [ ] **Step 1: Write the fixture**

`tests/fixtures/receipts/netflix/renewal.txt` — paste a redacted Netflix renewal email (subject, from, plain-text body). Minimum content the parser depends on:

```
From: Netflix <info@account.netflix.com>
Subject: Your Netflix payment

Hi Volodymyr,

We just charged your account.

Plan: Netflix Premium
Total: $15.99 USD
Date: May 18, 2026
```

- [ ] **Step 2: Write the failing test**

`src/infrastructure/email/parsers/netflix.rs`:

```rust
use chrono::{DateTime, TimeZone, Utc};
use regex::Regex;
use rust_decimal::Decimal;
use std::str::FromStr;

use crate::domain::email::RawEmail;
use crate::domain::receipt_parser::{ParsedReceipt, ReceiptParser};
use crate::domain::subscription::{BillingPeriod, SubscriptionProvider};
use crate::domain::subscription_charge::ReceiptKind;

pub struct NetflixParser;

impl NetflixParser {
    pub fn new() -> Self {
        Self
    }
}

impl ReceiptParser for NetflixParser {
    fn matches_sender(&self, from: &str) -> bool {
        from.to_ascii_lowercase().contains("info@account.netflix.com")
    }

    fn parse(&self, email: &RawEmail) -> anyhow::Result<Option<ParsedReceipt>> {
        let body = match email.body_text.as_deref() {
            Some(b) => b,
            None => return Ok(None),
        };
        let plan_re = Regex::new(r"(?i)Plan:\s*(.+)").unwrap();
        let total_re = Regex::new(r"(?i)Total:\s*\$?([0-9]+(?:\.[0-9]{2})?)\s*([A-Z]{3})").unwrap();
        let date_re = Regex::new(r"(?i)Date:\s*([A-Za-z]+ [0-9]{1,2}, [0-9]{4})").unwrap();

        let plan = plan_re
            .captures(body)
            .and_then(|c| c.get(1).map(|m| m.as_str().trim().to_string()))
            .ok_or_else(|| anyhow::anyhow!("plan not found"))?;
        let total = total_re
            .captures(body)
            .ok_or_else(|| anyhow::anyhow!("total not found"))?;
        let amount = Decimal::from_str(total.get(1).unwrap().as_str())?;
        let currency = total.get(2).unwrap().as_str().to_string();
        let date_str = date_re
            .captures(body)
            .and_then(|c| c.get(1).map(|m| m.as_str().to_string()))
            .ok_or_else(|| anyhow::anyhow!("date not found"))?;
        let charged_at = parse_us_date(&date_str)?;

        let merchant_key = format!("netflix.com:{}", plan.to_ascii_lowercase().replace(' ', "_"));

        Ok(Some(ParsedReceipt {
            provider: SubscriptionProvider::Netflix,
            product_name: plan,
            merchant_key,
            amount,
            currency,
            charged_at,
            billing_period_hint: Some(BillingPeriod::Monthly),
            kind: ReceiptKind::Renewal,
        }))
    }
}

fn parse_us_date(s: &str) -> anyhow::Result<DateTime<Utc>> {
    let naive = chrono::NaiveDate::parse_from_str(s, "%B %d, %Y")?;
    let dt = naive.and_hms_opt(0, 0, 0).unwrap();
    Ok(Utc.from_utc_datetime(&dt))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_email() -> RawEmail {
        let body = std::fs::read_to_string("tests/fixtures/receipts/netflix/renewal.txt").unwrap();
        RawEmail {
            message_id: "<netflix-renewal-1>".to_string(),
            from: "Netflix <info@account.netflix.com>".to_string(),
            subject: "Your Netflix payment".to_string(),
            received_at: Utc::now(),
            body_text: Some(body),
            body_html: None,
        }
    }

    #[test]
    fn matches_sender_case_insensitive() {
        let p = NetflixParser::new();
        assert!(p.matches_sender("Netflix <info@account.netflix.com>"));
        assert!(p.matches_sender("INFO@ACCOUNT.NETFLIX.COM"));
        assert!(!p.matches_sender("noreply@hulu.com"));
    }

    #[test]
    fn parses_renewal_fixture() {
        let p = NetflixParser::new();
        let r = p.parse(&fixture_email()).unwrap().unwrap();
        assert_eq!(r.provider, SubscriptionProvider::Netflix);
        assert_eq!(r.product_name, "Netflix Premium");
        assert_eq!(r.merchant_key, "netflix.com:netflix_premium");
        assert_eq!(r.amount.to_string(), "15.99");
        assert_eq!(r.currency, "USD");
        assert_eq!(r.kind, ReceiptKind::Renewal);
        assert_eq!(r.billing_period_hint, Some(BillingPeriod::Monthly));
    }

    #[test]
    fn returns_none_when_body_missing() {
        let p = NetflixParser::new();
        let mut e = fixture_email();
        e.body_text = None;
        assert!(p.parse(&e).unwrap().is_none());
    }
}
```

- [ ] **Step 3: Create the module skeleton files**

`src/infrastructure/email/mod.rs`:

```rust
pub mod parsers;
```

`src/infrastructure/email/parsers/mod.rs`:

```rust
pub mod netflix;
```

Edit `src/infrastructure/mod.rs` — add `pub mod email;`.

- [ ] **Step 4: Run tests, verify fail then pass**

Run: `cargo test infrastructure::email::parsers::netflix`
Expected: 3 tests PASS.

- [ ] **Step 5: Commit**

```bash
git add src/infrastructure/email tests/fixtures/receipts/netflix src/infrastructure/mod.rs
git commit -m "feat(infra): NetflixParser"
```

---

## Task 15: Parser — GooglePlayParser

**Files:**
- Create: `src/infrastructure/email/parsers/google_play.rs`
- Create: `tests/fixtures/receipts/google_play/renewal.html`
- Modify: `src/infrastructure/email/parsers/mod.rs`

- [ ] **Step 1: Write the fixture**

Save a redacted Google Play receipt HTML. The relevant content the parser pulls:

- App / item name (e.g. `Notion - Notes & Productivity`)
- Subscription period hint (e.g. `Monthly`)
- Price (e.g. `$9.99` or `UAH 379.00`)
- Order date

Minimum fixture content:

```html
<html><body>
<p>Order completed: May 18, 2026</p>
<p>Item: Notion - Notes &amp; Productivity (Monthly)</p>
<p>Price: $9.99</p>
</body></html>
```

- [ ] **Step 2: Write the parser**

```rust
use chrono::{DateTime, TimeZone, Utc};
use regex::Regex;
use rust_decimal::Decimal;
use scraper::{Html, Selector};
use std::str::FromStr;

use crate::domain::email::RawEmail;
use crate::domain::receipt_parser::{ParsedReceipt, ReceiptParser};
use crate::domain::subscription::{BillingPeriod, SubscriptionProvider};
use crate::domain::subscription_charge::ReceiptKind;

pub struct GooglePlayParser;

impl GooglePlayParser {
    pub fn new() -> Self {
        Self
    }
}

impl ReceiptParser for GooglePlayParser {
    fn matches_sender(&self, from: &str) -> bool {
        from.to_ascii_lowercase().contains("googleplay-noreply@google.com")
    }

    fn parse(&self, email: &RawEmail) -> anyhow::Result<Option<ParsedReceipt>> {
        let html = match email.body_html.as_deref() {
            Some(h) => h,
            None => return Ok(None),
        };
        let doc = Html::parse_document(html);
        let p_sel = Selector::parse("p").unwrap();
        let mut text = String::new();
        for el in doc.select(&p_sel) {
            text.push_str(&el.text().collect::<String>());
            text.push('\n');
        }

        let item_re = Regex::new(r"(?i)Item:\s*(.+?)\s*\(([^)]+)\)").unwrap();
        let price_re = Regex::new(r"(?i)Price:\s*(\$|[A-Z]{3} )?([0-9]+(?:\.[0-9]{2})?)").unwrap();
        let date_re = Regex::new(r"(?i)Order completed:\s*([A-Za-z]+ [0-9]{1,2}, [0-9]{4})").unwrap();

        let item_caps = item_re.captures(&text).ok_or_else(|| anyhow::anyhow!("item not found"))?;
        let product_name = item_caps.get(1).unwrap().as_str().trim().to_string();
        let period_str = item_caps.get(2).unwrap().as_str().to_ascii_lowercase();
        let billing_period_hint = match period_str.as_str() {
            "monthly" => Some(BillingPeriod::Monthly),
            "yearly" | "annual" => Some(BillingPeriod::Yearly),
            "weekly" => Some(BillingPeriod::Weekly),
            _ => None,
        };

        let price_caps = price_re.captures(&text).ok_or_else(|| anyhow::anyhow!("price not found"))?;
        let prefix = price_caps.get(1).map(|m| m.as_str().trim().to_string());
        let amount = Decimal::from_str(price_caps.get(2).unwrap().as_str())?;
        let currency = match prefix.as_deref() {
            Some("$") => "USD".to_string(),
            Some(other) => other.to_string(),
            None => "USD".to_string(),
        };

        let date_str = date_re
            .captures(&text)
            .and_then(|c| c.get(1).map(|m| m.as_str().to_string()))
            .ok_or_else(|| anyhow::anyhow!("date not found"))?;
        let naive = chrono::NaiveDate::parse_from_str(&date_str, "%B %d, %Y")?;
        let charged_at = Utc.from_utc_datetime(&naive.and_hms_opt(0, 0, 0).unwrap());

        let merchant_key = format!(
            "play.google.com:{}",
            product_name.to_ascii_lowercase().replace(' ', "_")
        );

        Ok(Some(ParsedReceipt {
            provider: SubscriptionProvider::GooglePlay,
            product_name,
            merchant_key,
            amount,
            currency,
            charged_at,
            billing_period_hint,
            kind: ReceiptKind::Renewal,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_email() -> RawEmail {
        let html = std::fs::read_to_string("tests/fixtures/receipts/google_play/renewal.html").unwrap();
        RawEmail {
            message_id: "<gp-1>".to_string(),
            from: "Google Play <googleplay-noreply@google.com>".to_string(),
            subject: "Your Google Play Order Receipt".to_string(),
            received_at: Utc::now(),
            body_text: None,
            body_html: Some(html),
        }
    }

    #[test]
    fn matches_sender() {
        let p = GooglePlayParser::new();
        assert!(p.matches_sender("Google Play <googleplay-noreply@google.com>"));
        assert!(!p.matches_sender("info@account.netflix.com"));
    }

    #[test]
    fn parses_renewal_fixture() {
        let p = GooglePlayParser::new();
        let r = p.parse(&fixture_email()).unwrap().unwrap();
        assert_eq!(r.provider, SubscriptionProvider::GooglePlay);
        assert!(r.product_name.starts_with("Notion"));
        assert_eq!(r.amount.to_string(), "9.99");
        assert_eq!(r.currency, "USD");
        assert_eq!(r.billing_period_hint, Some(BillingPeriod::Monthly));
    }
}
```

- [ ] **Step 3: Register submodule**

Edit `src/infrastructure/email/parsers/mod.rs` — add `pub mod google_play;`.

- [ ] **Step 4: Run tests**

Run: `cargo test infrastructure::email::parsers::google_play`
Expected: 2 tests PASS.

- [ ] **Step 5: Commit**

```bash
git add src/infrastructure/email/parsers/google_play.rs tests/fixtures/receipts/google_play src/infrastructure/email/parsers/mod.rs
git commit -m "feat(infra): GooglePlayParser"
```

---

## Task 16: Parser — AppleParser

**Files:**
- Create: `src/infrastructure/email/parsers/apple.rs`
- Create: `tests/fixtures/receipts/apple/renewal.html`
- Modify: `src/infrastructure/email/parsers/mod.rs`

Apple receipts vary widely but consistently include "App Name", "Subscription", price, and date. Use the same structure as `GooglePlayParser` but adapt selectors/regexes to your fixture.

- [ ] **Step 1: Save a redacted Apple receipt HTML fixture** at `tests/fixtures/receipts/apple/renewal.html` containing at minimum app name, subscription period, price, and date.

- [ ] **Step 2: Write `AppleParser`** implementing `ReceiptParser`. Sender match: `no_reply@email.apple.com`. Merchant key prefix: `apps.apple.com:`. Provider: `SubscriptionProvider::AppleAppStore`. Tests mirroring the Netflix/Google Play parser test modules (sender match + fixture parse + missing-body returns None).

- [ ] **Step 3: Register submodule** — edit `src/infrastructure/email/parsers/mod.rs` to add `pub mod apple;`.

- [ ] **Step 4: Run tests**

Run: `cargo test infrastructure::email::parsers::apple`
Expected: 2-3 tests PASS.

- [ ] **Step 5: Commit**

```bash
git add src/infrastructure/email/parsers/apple.rs tests/fixtures/receipts/apple src/infrastructure/email/parsers/mod.rs
git commit -m "feat(infra): AppleParser"
```

---

## Task 17: Parser registry

**Files:**
- Modify: `src/infrastructure/email/parsers/mod.rs`

- [ ] **Step 1: Write failing test**

Add at the bottom of `mod.rs`:

```rust
use crate::domain::receipt_parser::ReceiptParser;

pub struct ParserRegistry {
    parsers: Vec<Box<dyn ReceiptParser>>,
}

impl ParserRegistry {
    pub fn default_set() -> Self {
        Self {
            parsers: vec![
                Box::new(netflix::NetflixParser::new()),
                Box::new(google_play::GooglePlayParser::new()),
                Box::new(apple::AppleParser::new()),
            ],
        }
    }

    pub fn find(&self, from: &str) -> Option<&dyn ReceiptParser> {
        self.parsers
            .iter()
            .find(|p| p.matches_sender(from))
            .map(|b| b.as_ref())
    }
}

#[cfg(test)]
mod registry_tests {
    use super::*;

    #[test]
    fn finds_netflix() {
        let reg = ParserRegistry::default_set();
        assert!(reg.find("info@account.netflix.com").is_some());
    }

    #[test]
    fn finds_google_play() {
        let reg = ParserRegistry::default_set();
        assert!(reg.find("googleplay-noreply@google.com").is_some());
    }

    #[test]
    fn finds_none_for_unknown() {
        let reg = ParserRegistry::default_set();
        assert!(reg.find("noreply@hulu.com").is_none());
    }
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test infrastructure::email::parsers::registry_tests`
Expected: 3 tests PASS.

- [ ] **Step 3: Commit**

```bash
git add src/infrastructure/email/parsers/mod.rs
git commit -m "feat(infra): ParserRegistry"
```

---

## Task 18: Infra — GmailClient (EmailFetcher impl)

**Files:**
- Create: `src/infrastructure/email/gmail_client.rs`
- Modify: `src/infrastructure/email/mod.rs`
- Create: `src/infrastructure/email/oauth.rs`

The Gmail client is the only piece that talks to the real Google API. We split out OAuth-token refresh so it can be unit-tested independently.

- [ ] **Step 1: Write `oauth.rs` — the refresh helper**

```rust
use chrono::{DateTime, Duration, Utc};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct RefreshResponse {
    access_token: String,
    expires_in: i64,
    refresh_token: Option<String>, // Google may or may not return a new one
}

#[derive(Debug, Clone)]
pub struct RefreshedTokens {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: DateTime<Utc>,
}

pub async fn refresh_gmail_token(
    http: &reqwest::Client,
    client_id: &str,
    client_secret: &str,
    refresh_token: &str,
) -> anyhow::Result<RefreshedTokens> {
    let res = http
        .post("https://oauth2.googleapis.com/token")
        .form(&[
            ("client_id", client_id),
            ("client_secret", client_secret),
            ("refresh_token", refresh_token),
            ("grant_type", "refresh_token"),
        ])
        .send()
        .await?
        .error_for_status()?
        .json::<RefreshResponse>()
        .await?;
    Ok(RefreshedTokens {
        access_token: res.access_token,
        refresh_token: res.refresh_token.unwrap_or_else(|| refresh_token.to_string()),
        expires_at: Utc::now() + Duration::seconds(res.expires_in - 60),
    })
}
```

- [ ] **Step 2: Write `gmail_client.rs`**

```rust
use async_trait::async_trait;
use base64::Engine;
use chrono::{DateTime, Utc};
use serde::Deserialize;

use crate::domain::email::{EmailFetcher, RawEmail};
use crate::domain::email_connection::EmailConnection;
use crate::infrastructure::email::oauth::{refresh_gmail_token, RefreshedTokens};

const SENDER_QUERY: &str = "from:(googleplay-noreply@google.com OR info@account.netflix.com OR no_reply@email.apple.com) newer_than:30d";

pub struct GmailClient {
    http: reqwest::Client,
    client_id: String,
    client_secret: String,
}

impl GmailClient {
    pub fn new(client_id: String, client_secret: String) -> Self {
        Self {
            http: reqwest::Client::new(),
            client_id,
            client_secret,
        }
    }

    async fn ensure_access_token(
        &self,
        conn: &EmailConnection,
    ) -> anyhow::Result<(String, Option<RefreshedTokens>)> {
        if conn.access_token_expires_at > Utc::now() + chrono::Duration::seconds(30) {
            return Ok((conn.oauth_access_token.clone(), None));
        }
        let refreshed = refresh_gmail_token(
            &self.http,
            &self.client_id,
            &self.client_secret,
            &conn.oauth_refresh_token,
        )
        .await?;
        Ok((refreshed.access_token.clone(), Some(refreshed)))
    }
}

#[derive(Deserialize)]
struct MessagesListResponse {
    messages: Option<Vec<MessageRef>>,
    #[serde(rename = "nextPageToken")]
    next_page_token: Option<String>,
}

#[derive(Deserialize)]
struct MessageRef {
    id: String,
}

#[derive(Deserialize)]
struct MessageFull {
    id: String,
    payload: Payload,
    #[serde(rename = "internalDate")]
    internal_date: String,
}

#[derive(Deserialize)]
struct Payload {
    headers: Vec<Header>,
    body: Option<Body>,
    parts: Option<Vec<Payload>>,
    #[serde(rename = "mimeType")]
    mime_type: String,
}

#[derive(Deserialize)]
struct Header {
    name: String,
    value: String,
}

#[derive(Deserialize)]
struct Body {
    data: Option<String>,
    size: Option<i64>,
}

fn header(payload: &Payload, name: &str) -> Option<String> {
    payload
        .headers
        .iter()
        .find(|h| h.name.eq_ignore_ascii_case(name))
        .map(|h| h.value.clone())
}

fn decode_body(body: &Body) -> Option<String> {
    let raw = body.data.as_ref()?;
    let bytes = base64::engine::general_purpose::URL_SAFE.decode(raw).ok()?;
    String::from_utf8(bytes).ok()
}

fn collect_bodies(payload: &Payload, text: &mut Option<String>, html: &mut Option<String>) {
    if let Some(body) = payload.body.as_ref() {
        if body.size.unwrap_or(0) > 0 {
            if payload.mime_type == "text/plain" && text.is_none() {
                *text = decode_body(body);
            } else if payload.mime_type == "text/html" && html.is_none() {
                *html = decode_body(body);
            }
        }
    }
    if let Some(parts) = payload.parts.as_ref() {
        for p in parts {
            collect_bodies(p, text, html);
        }
    }
}

fn payload_to_raw(msg: MessageFull) -> anyhow::Result<RawEmail> {
    let from = header(&msg.payload, "From").unwrap_or_default();
    let subject = header(&msg.payload, "Subject").unwrap_or_default();
    let message_id = header(&msg.payload, "Message-ID")
        .or_else(|| header(&msg.payload, "Message-Id"))
        .unwrap_or_else(|| msg.id.clone());
    let received_at = msg
        .internal_date
        .parse::<i64>()
        .ok()
        .and_then(|ms| DateTime::<Utc>::from_timestamp(ms / 1000, 0))
        .unwrap_or_else(Utc::now);
    let mut text = None;
    let mut html = None;
    collect_bodies(&msg.payload, &mut text, &mut html);
    Ok(RawEmail {
        message_id,
        from,
        subject,
        received_at,
        body_text: text,
        body_html: html,
    })
}

#[async_trait]
impl EmailFetcher for GmailClient {
    async fn fetch_new(
        &self,
        conn: &EmailConnection,
    ) -> anyhow::Result<(Vec<RawEmail>, Option<String>)> {
        let (access_token, _refreshed) = self.ensure_access_token(conn).await?;
        // Note: token refresh-and-persist happens in SyncEmailUseCase, which can write to the repo.
        // Here we just use the returned access_token for this call.

        // For v1 we use messages.list with the sender query, ignoring history API complexity.
        let mut message_ids: Vec<String> = Vec::new();
        let mut page_token: Option<String> = None;

        loop {
            let mut url = format!(
                "https://gmail.googleapis.com/gmail/v1/users/me/messages?q={}",
                urlencoding::encode(SENDER_QUERY)
            );
            if let Some(tok) = &page_token {
                url.push_str(&format!("&pageToken={tok}"));
            }
            let resp: MessagesListResponse = self
                .http
                .get(&url)
                .bearer_auth(&access_token)
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;
            if let Some(msgs) = resp.messages {
                message_ids.extend(msgs.into_iter().map(|m| m.id));
            }
            match resp.next_page_token {
                Some(t) => page_token = Some(t),
                None => break,
            }
        }

        let mut out = Vec::new();
        for id in message_ids {
            let url = format!(
                "https://gmail.googleapis.com/gmail/v1/users/me/messages/{id}?format=full"
            );
            let msg: MessageFull = self
                .http
                .get(&url)
                .bearer_auth(&access_token)
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;
            out.push(payload_to_raw(msg)?);
        }

        // Cursor: we don't use historyId in v1; pass through whatever was stored.
        Ok((out, conn.last_history_id.clone()))
    }
}
```

Add to `Cargo.toml`:

```toml
base64 = "0.22"
urlencoding = "2"
```

- [ ] **Step 3: Register module**

Edit `src/infrastructure/email/mod.rs`:

```rust
pub mod gmail_client;
pub mod oauth;
pub mod parsers;
```

- [ ] **Step 4: Verify build**

Run: `cargo build`
Expected: PASS.

(The Gmail client itself is not unit-tested in this task — it talks to the network. Use-case tests in later tasks will exercise it via a fake `EmailFetcher`. An env-gated live test is suggested but out of scope for this plan.)

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock src/infrastructure/email
git commit -m "feat(infra): GmailClient implementing EmailFetcher"
```

---

## Task 19: Application — SubscriptionService scaffold + ConnectGmailUseCase

**Files:**
- Create: `src/application/subscriptions.rs`
- Modify: `src/application/mod.rs`

- [ ] **Step 1: Write the service skeleton + connect use case**

```rust
use std::sync::Arc;

use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::domain::email_connection::{
    EmailConnection, EmailConnectionRepository, EmailConnectionStatus, EmailProvider,
};
use crate::domain::subscription_error::SubscriptionError;

pub struct ConnectGmailParams {
    pub email_address: String,
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: DateTime<Utc>,
}

pub struct SubscriptionService {
    pub connections: Arc<dyn EmailConnectionRepository>,
}

impl SubscriptionService {
    pub fn new(connections: Arc<dyn EmailConnectionRepository>) -> Self {
        Self { connections }
    }

    pub async fn connect_gmail(
        &self,
        user_id: Uuid,
        params: ConnectGmailParams,
    ) -> anyhow::Result<EmailConnection> {
        let conn = EmailConnection {
            id: Uuid::new_v4(),
            user_id,
            provider: EmailProvider::Gmail,
            email_address: params.email_address,
            oauth_access_token: params.access_token,
            oauth_refresh_token: params.refresh_token,
            access_token_expires_at: params.expires_at,
            status: EmailConnectionStatus::Connected,
            last_synced_at: None,
            last_history_id: None,
            created_at: Utc::now(),
        };
        self.connections.create(&conn).await?;
        Ok(conn)
    }

    pub async fn list_connections(
        &self,
        user_id: Uuid,
    ) -> anyhow::Result<Vec<EmailConnection>> {
        self.connections.list_by_user(user_id).await
    }

    pub async fn delete_connection(&self, id: Uuid, user_id: Uuid) -> anyhow::Result<()> {
        let exists = self.connections.find_by_id(id, user_id).await?.is_some();
        if !exists {
            return Err(SubscriptionError::ConnectionNotFound.into());
        }
        self.connections.delete(id, user_id).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infrastructure::email_connection_repository::PgEmailConnectionRepository;
    use crate::infrastructure::test_db;

    #[tokio::test]
    async fn connect_gmail_persists_connected_status() {
        let pool = test_db::fresh_pool().await;
        let repo: Arc<dyn EmailConnectionRepository> =
            Arc::new(PgEmailConnectionRepository::new(pool));
        let svc = SubscriptionService::new(repo.clone());
        let user_id = Uuid::new_v4();
        let conn = svc
            .connect_gmail(
                user_id,
                ConnectGmailParams {
                    email_address: "x@y.com".into(),
                    access_token: "a".into(),
                    refresh_token: "r".into(),
                    expires_at: Utc::now() + chrono::Duration::hours(1),
                },
            )
            .await
            .unwrap();
        assert_eq!(conn.status, EmailConnectionStatus::Connected);
        let found = repo.find_by_id(conn.id, user_id).await.unwrap().unwrap();
        assert_eq!(found.email_address, "x@y.com");
    }

    #[tokio::test]
    async fn delete_returns_error_when_missing() {
        let pool = test_db::fresh_pool().await;
        let repo: Arc<dyn EmailConnectionRepository> =
            Arc::new(PgEmailConnectionRepository::new(pool));
        let svc = SubscriptionService::new(repo);
        let err = svc
            .delete_connection(Uuid::new_v4(), Uuid::new_v4())
            .await
            .unwrap_err();
        assert!(err.downcast_ref::<SubscriptionError>().is_some());
    }
}
```

- [ ] **Step 2: Register module**

Edit `src/application/mod.rs` — add `pub mod subscriptions;`.

- [ ] **Step 3: Run tests**

Run: `cargo test application::subscriptions`
Expected: 2 tests PASS.

- [ ] **Step 4: Commit**

```bash
git add src/application/subscriptions.rs src/application/mod.rs
git commit -m "feat(app): SubscriptionService scaffold + ConnectGmailUseCase"
```

---

## Task 20: Application — SyncEmailUseCase

**Files:**
- Modify: `src/application/subscriptions.rs`

- [ ] **Step 1: Add fields and `sync_connection` method to `SubscriptionService`**

```rust
// Add to imports:
use rust_decimal::Decimal;
use crate::domain::email::{EmailFetcher, RawEmail};
use crate::domain::receipt_parser::ParsedReceipt;
use crate::domain::subscription::{
    BillingPeriod, Subscription, SubscriptionProvider, SubscriptionRepository, SubscriptionStatus,
};
use crate::domain::subscription_charge::{
    ChargeMatchStatus, ReceiptKind, SubscriptionCharge, SubscriptionChargeRepository,
};
use crate::infrastructure::email::parsers::ParserRegistry;

// Update struct:
pub struct SubscriptionService {
    pub connections: Arc<dyn EmailConnectionRepository>,
    pub subscriptions: Arc<dyn SubscriptionRepository>,
    pub charges: Arc<dyn SubscriptionChargeRepository>,
    pub fetcher: Arc<dyn EmailFetcher>,
    pub parsers: Arc<ParserRegistry>,
}

impl SubscriptionService {
    pub fn new(
        connections: Arc<dyn EmailConnectionRepository>,
        subscriptions: Arc<dyn SubscriptionRepository>,
        charges: Arc<dyn SubscriptionChargeRepository>,
        fetcher: Arc<dyn EmailFetcher>,
        parsers: Arc<ParserRegistry>,
    ) -> Self {
        Self { connections, subscriptions, charges, fetcher, parsers }
    }

    /// Sync one connection. Returns the list of newly-inserted charge ids
    /// (to be passed to the matcher).
    pub async fn sync_connection(&self, conn_id: Uuid) -> anyhow::Result<Vec<Uuid>> {
        let conn = self
            .connections
            .list_by_user(Uuid::nil()) // not the right query — placeholder removed below
            .await?
            .into_iter()
            .find(|c| c.id == conn_id);
        // Replace the above with a proper getter — see step 2.
        let conn = conn.ok_or(SubscriptionError::ConnectionNotFound)?;

        let (emails, new_cursor) = self.fetcher.fetch_new(&conn).await?;
        let mut new_charge_ids = Vec::new();
        for email in emails {
            let Some(parser) = self.parsers.find(&email.from) else { continue };
            let Some(receipt) = parser.parse(&email)? else { continue };
            let sub = self
                .upsert_subscription_from_receipt(conn.user_id, &receipt)
                .await?;
            let charge = SubscriptionCharge {
                id: Uuid::new_v4(),
                subscription_id: sub.id,
                user_id: conn.user_id,
                amount: receipt.amount,
                currency: receipt.currency.clone(),
                charged_at: receipt.charged_at,
                email_message_id: email.message_id.clone(),
                kind: receipt.kind.clone(),
                transaction_id: None,
                match_status: ChargeMatchStatus::Pending,
                created_at: Utc::now(),
            };
            let (id, inserted) = self.charges.create_idempotent(&charge).await?;
            if inserted {
                new_charge_ids.push(id);
            }
        }
        self.connections
            .update_sync_cursor(conn.id, Utc::now(), new_cursor)
            .await?;
        Ok(new_charge_ids)
    }

    async fn upsert_subscription_from_receipt(
        &self,
        user_id: Uuid,
        receipt: &ParsedReceipt,
    ) -> anyhow::Result<Subscription> {
        let billing_period = receipt.billing_period_hint.unwrap_or(BillingPeriod::Monthly);
        let now = Utc::now();
        let sub = Subscription {
            id: Uuid::new_v4(),
            user_id,
            provider: receipt.provider.clone(),
            product_name: receipt.product_name.clone(),
            merchant_key: receipt.merchant_key.clone(),
            amount: receipt.amount,
            currency: receipt.currency.clone(),
            billing_period,
            status: SubscriptionStatus::Active,
            started_at: receipt.charged_at,
            last_charged_at: None,
            next_expected_at: None,
            category_id: None,
            created_at: now,
        };
        self.subscriptions.upsert_by_merchant_key(&sub).await
    }
}
```

- [ ] **Step 2: Add a `find_by_id` shortcut on `EmailConnectionRepository`**

Replace the placeholder lookup in `sync_connection` with a direct repository query. Add a helper to `SubscriptionService`:

```rust
async fn get_connection(&self, conn_id: Uuid) -> anyhow::Result<EmailConnection> {
    // We don't yet know the user_id — add a connection-only lookup.
    self.find_connection(conn_id)
        .await?
        .ok_or_else(|| SubscriptionError::ConnectionNotFound.into())
}

async fn find_connection(&self, conn_id: Uuid) -> anyhow::Result<Option<EmailConnection>> {
    // Scan list_connected since IDs are globally unique; this is the simplest
    // option without adding a new repo method. For higher scale, add a
    // `find_by_id_no_user(id)` to the repo.
    Ok(self
        .connections
        .list_connected()
        .await?
        .into_iter()
        .find(|c| c.id == conn_id))
}
```

(If `list_connected` becomes hot, add a `find_by_id_no_user` repo method. For v1 the iteration is fine.)

- [ ] **Step 3: Add a test using a fake fetcher**

Append to the test module in `subscriptions.rs`:

```rust
#[cfg(test)]
mod sync_tests {
    use super::*;
    use crate::infrastructure::email_connection_repository::PgEmailConnectionRepository;
    use crate::infrastructure::subscription_charge_repository::PgSubscriptionChargeRepository;
    use crate::infrastructure::subscription_repository::PgSubscriptionRepository;
    use crate::infrastructure::test_db;

    struct FakeFetcher {
        emails: Vec<RawEmail>,
    }
    #[async_trait::async_trait]
    impl EmailFetcher for FakeFetcher {
        async fn fetch_new(
            &self,
            _conn: &EmailConnection,
        ) -> anyhow::Result<(Vec<RawEmail>, Option<String>)> {
            Ok((self.emails.clone(), Some("cursor-1".to_string())))
        }
    }

    fn netflix_email(msg_id: &str) -> RawEmail {
        RawEmail {
            message_id: msg_id.to_string(),
            from: "Netflix <info@account.netflix.com>".into(),
            subject: "Your Netflix payment".into(),
            received_at: Utc::now(),
            body_text: Some(
                "Plan: Netflix Premium\nTotal: $15.99 USD\nDate: May 18, 2026".into(),
            ),
            body_html: None,
        }
    }

    #[tokio::test]
    async fn sync_creates_subscription_and_charge_then_is_idempotent() {
        let pool = test_db::fresh_pool().await;
        let user_id = Uuid::new_v4();
        let conns: Arc<dyn EmailConnectionRepository> =
            Arc::new(PgEmailConnectionRepository::new(pool.clone()));
        let subs: Arc<dyn SubscriptionRepository> =
            Arc::new(PgSubscriptionRepository::new(pool.clone()));
        let charges: Arc<dyn SubscriptionChargeRepository> =
            Arc::new(PgSubscriptionChargeRepository::new(pool.clone()));
        let svc = SubscriptionService::new(
            conns.clone(),
            subs,
            charges.clone(),
            Arc::new(FakeFetcher {
                emails: vec![netflix_email("<m1>")],
            }),
            Arc::new(ParserRegistry::default_set()),
        );

        let conn = svc
            .connect_gmail(
                user_id,
                ConnectGmailParams {
                    email_address: "x@y.com".into(),
                    access_token: "a".into(),
                    refresh_token: "r".into(),
                    expires_at: Utc::now() + chrono::Duration::hours(1),
                },
            )
            .await
            .unwrap();

        let ids1 = svc.sync_connection(conn.id).await.unwrap();
        assert_eq!(ids1.len(), 1);

        // Second sync with the same email → 0 new ids (idempotent on email_message_id).
        let ids2 = svc.sync_connection(conn.id).await.unwrap();
        assert!(ids2.is_empty());

        let pending = charges.list_pending_for_user(user_id).await.unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].amount.to_string(), "15.99");
    }
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test application::subscriptions::sync_tests`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/application/subscriptions.rs
git commit -m "feat(app): SyncEmailUseCase with idempotent charge ingestion"
```

---

## Task 21: Application — MatchChargesUseCase

**Files:**
- Create: `src/application/subscription_matching.rs`
- Modify: `src/application/mod.rs`
- Modify: `src/domain/transaction.rs` — add a list-by-time-and-amount helper, OR add a method to the repo trait.

- [ ] **Step 1: Add a candidate query to `TransactionRepository`**

In `src/domain/transaction.rs`, append to the trait:

```rust
/// Returns expense transactions for `user_id` whose `transacted_at` falls in
/// `[from, to]` and whose `amount` is within `[min_amount, max_amount]`,
/// excluding those already linked to a `subscription_charge`. Used by the
/// subscription matcher.
async fn list_match_candidates(
    &self,
    user_id: Uuid,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    min_amount: Decimal,
    max_amount: Decimal,
    currency: &str,
) -> anyhow::Result<Vec<Transaction>>;
```

Implement it in `src/infrastructure/transaction_repository.rs`:

```rust
async fn list_match_candidates(
    &self,
    user_id: Uuid,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    min_amount: Decimal,
    max_amount: Decimal,
    currency: &str,
) -> anyhow::Result<Vec<Transaction>> {
    let rows = sqlx::query_as::<_, TransactionRow>(
        "SELECT t.* FROM transactions t \
         LEFT JOIN subscription_charges sc ON sc.transaction_id = t.id \
         WHERE t.user_id = $1 \
           AND t.kind = 'Expense' \
           AND t.transacted_at BETWEEN $2 AND $3 \
           AND t.amount BETWEEN $4 AND $5 \
           AND t.currency = $6 \
           AND sc.id IS NULL",
    )
    .bind(user_id)
    .bind(from.timestamp())
    .bind(to.timestamp())
    .bind(min_amount)
    .bind(max_amount)
    .bind(currency)
    .fetch_all(&self.pool)
    .await?;
    rows.into_iter().map(row_to_tx).collect()
}
```

(Use the `TransactionRow` / `row_to_tx` already present in that file.)

- [ ] **Step 2: Write `MatchChargesUseCase`**

```rust
use std::sync::Arc;

use chrono::{Duration, Utc};
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use uuid::Uuid;

use crate::domain::account::AccountRepository;
use crate::domain::fx_rate::FxRateRepository;
use crate::domain::subscription::{BillingPeriod, SubscriptionRepository, SubscriptionStatus};
use crate::domain::subscription_charge::{
    ChargeMatchStatus, SubscriptionCharge, SubscriptionChargeRepository,
};
use crate::domain::transaction::{Transaction, TransactionRepository};

const AMOUNT_TOLERANCE_PCT: Decimal = Decimal::from_parts(5, 0, 0, false, 2); // 0.05
const TIME_WINDOW_DAYS: i64 = 3;
const UNMATCHED_AFTER_DAYS: i64 = 7;

pub struct MatchChargesUseCase {
    pub charges: Arc<dyn SubscriptionChargeRepository>,
    pub subscriptions: Arc<dyn SubscriptionRepository>,
    pub transactions: Arc<dyn TransactionRepository>,
    pub accounts: Arc<dyn AccountRepository>,
    pub fx: Arc<dyn FxRateRepository>,
}

impl MatchChargesUseCase {
    pub async fn run_for_user(&self, user_id: Uuid) -> anyhow::Result<()> {
        let pending = self.charges.list_pending_for_user(user_id).await?;
        for charge in pending {
            self.try_match_one(&charge).await?;
        }
        let threshold = Utc::now() - Duration::days(UNMATCHED_AFTER_DAYS);
        self.charges
            .mark_pending_older_than_unmatched(threshold)
            .await?;
        Ok(())
    }

    async fn try_match_one(&self, charge: &SubscriptionCharge) -> anyhow::Result<()> {
        let from = charge.charged_at - Duration::days(TIME_WINDOW_DAYS);
        let to = charge.charged_at + Duration::days(TIME_WINDOW_DAYS);

        // For v1 we restrict candidates to transactions in the SAME currency as
        // the receipt — FX conversion across currencies is a follow-up.
        // (See spec: the matcher converts amounts; we keep it lean here.)
        let bounds = amount_bounds(charge.amount);
        let candidates = self
            .transactions
            .list_match_candidates(
                charge.user_id,
                from,
                to,
                bounds.0,
                bounds.1,
                &charge.currency,
            )
            .await?;

        let Some(best) = pick_best(charge, &candidates) else {
            return Ok(());
        };

        self.charges
            .update_match(charge.id, Some(best.id), ChargeMatchStatus::Matched)
            .await?;

        // Update subscription bookkeeping and ensure status=Active.
        if let Some(sub) = self.subscriptions.find_by_id(charge.subscription_id, charge.user_id).await? {
            let next_expected = sub.billing_period.next_after(charge.charged_at);
            self.subscriptions
                .update_after_charge(
                    sub.id,
                    charge.charged_at,
                    next_expected,
                    SubscriptionStatus::Active,
                )
                .await?;

            // If the transaction has no category, write the sub's category.
            if let Some(cat) = sub.category_id {
                if best.category_id.is_none() {
                    let mut updated = best.clone();
                    updated.category_id = Some(cat);
                    self.transactions
                        .update(&updated, &crate::domain::transaction::TransactionDetails::None)
                        .await?;
                }
            }
        }
        Ok(())
    }
}

fn amount_bounds(amount: Decimal) -> (Decimal, Decimal) {
    let tol = amount * AMOUNT_TOLERANCE_PCT;
    (amount - tol, amount + tol)
}

fn pick_best<'a>(charge: &SubscriptionCharge, candidates: &'a [Transaction]) -> Option<&'a Transaction> {
    candidates.iter().min_by(|a, b| {
        let sa = score(charge, a);
        let sb = score(charge, b);
        sa.partial_cmp(&sb).unwrap_or(std::cmp::Ordering::Equal)
    })
}

fn score(charge: &SubscriptionCharge, tx: &Transaction) -> f64 {
    let amount_delta_pct = ((charge.amount - tx.amount) / charge.amount)
        .abs()
        .to_f64()
        .unwrap_or(1.0);
    let time_delta_h = (charge.charged_at - tx.transacted_at).num_hours().abs() as f64;
    amount_delta_pct + time_delta_h / 24.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::account::{Account, AccountDetails, AccountType};
    use crate::domain::subscription::{Subscription, SubscriptionProvider};
    use crate::domain::subscription_charge::ReceiptKind;
    use crate::domain::transaction::{TransactionDetails, TransactionKind};
    use crate::infrastructure::account_repository::SqliteAccountRepository;
    use crate::infrastructure::fx_rate_repository::PgFxRateRepository;
    use crate::infrastructure::subscription_charge_repository::PgSubscriptionChargeRepository;
    use crate::infrastructure::subscription_repository::PgSubscriptionRepository;
    use crate::infrastructure::test_db;
    use crate::infrastructure::transaction_repository::SqliteTransactionRepository;
    use rust_decimal_macros::dec;

    async fn setup() -> (sqlx::PgPool, Uuid, Uuid, MatchChargesUseCase) {
        let pool = test_db::fresh_pool().await;
        let user_id = Uuid::new_v4();

        let accounts: Arc<dyn AccountRepository> =
            Arc::new(SqliteAccountRepository::new(pool.clone()));
        let acc = Account::new(user_id, "Card".into(), AccountType::Cash, "USD".into());
        let account_id = acc.id;
        accounts.create(&acc, &AccountDetails::None).await.unwrap();

        let txs: Arc<dyn TransactionRepository> =
            Arc::new(SqliteTransactionRepository::new(pool.clone()));
        let subs: Arc<dyn SubscriptionRepository> =
            Arc::new(PgSubscriptionRepository::new(pool.clone()));
        let charges: Arc<dyn SubscriptionChargeRepository> =
            Arc::new(PgSubscriptionChargeRepository::new(pool.clone()));
        let fx: Arc<dyn FxRateRepository> = Arc::new(PgFxRateRepository::new(pool.clone()));

        let uc = MatchChargesUseCase {
            charges,
            subscriptions: subs,
            transactions: txs,
            accounts,
            fx,
        };
        (pool, user_id, account_id, uc)
    }

    fn sub(user_id: Uuid) -> Subscription {
        Subscription {
            id: Uuid::new_v4(),
            user_id,
            provider: SubscriptionProvider::Netflix,
            product_name: "Netflix Premium".into(),
            merchant_key: "netflix.com:premium".into(),
            amount: dec!(15.99),
            currency: "USD".into(),
            billing_period: BillingPeriod::Monthly,
            status: SubscriptionStatus::Active,
            started_at: Utc::now(),
            last_charged_at: None,
            next_expected_at: None,
            category_id: None,
            created_at: Utc::now(),
        }
    }

    #[tokio::test]
    async fn matches_within_amount_and_time_window() {
        let (_pool, user_id, account_id, uc) = setup().await;
        let s = sub(user_id);
        uc.subscriptions.upsert_by_merchant_key(&s).await.unwrap();

        let charge_time = Utc::now();
        let charge = SubscriptionCharge {
            id: Uuid::new_v4(),
            subscription_id: s.id,
            user_id,
            amount: dec!(15.99),
            currency: "USD".into(),
            charged_at: charge_time,
            email_message_id: "msg-1".into(),
            kind: ReceiptKind::Renewal,
            transaction_id: None,
            match_status: ChargeMatchStatus::Pending,
            created_at: Utc::now(),
        };
        uc.charges.create_idempotent(&charge).await.unwrap();

        let tx = Transaction::new(
            account_id,
            user_id,
            dec!(16.00),
            "USD".into(),
            TransactionKind::Expense,
            None,
            None,
            charge_time + Duration::hours(2),
        );
        uc.transactions.create(&tx, &TransactionDetails::None).await.unwrap();

        uc.run_for_user(user_id).await.unwrap();

        let found = uc
            .charges
            .find_by_id(charge.id, user_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(found.match_status, ChargeMatchStatus::Matched);
        assert_eq!(found.transaction_id, Some(tx.id));
    }

    #[tokio::test]
    async fn no_match_when_outside_tolerance() {
        let (_pool, user_id, account_id, uc) = setup().await;
        let s = sub(user_id);
        uc.subscriptions.upsert_by_merchant_key(&s).await.unwrap();

        let charge = SubscriptionCharge {
            id: Uuid::new_v4(),
            subscription_id: s.id,
            user_id,
            amount: dec!(15.99),
            currency: "USD".into(),
            charged_at: Utc::now(),
            email_message_id: "msg-2".into(),
            kind: ReceiptKind::Renewal,
            transaction_id: None,
            match_status: ChargeMatchStatus::Pending,
            created_at: Utc::now(),
        };
        uc.charges.create_idempotent(&charge).await.unwrap();

        // tx is 20% off — outside ±5%
        let tx = Transaction::new(
            account_id,
            user_id,
            dec!(20.00),
            "USD".into(),
            TransactionKind::Expense,
            None,
            None,
            Utc::now(),
        );
        uc.transactions.create(&tx, &TransactionDetails::None).await.unwrap();

        uc.run_for_user(user_id).await.unwrap();
        let still_pending = uc.charges.find_by_id(charge.id, user_id).await.unwrap().unwrap();
        assert_eq!(still_pending.match_status, ChargeMatchStatus::Pending);
    }
}
```

- [ ] **Step 3: Register module + Cargo dev-dep**

`Cargo.toml` `[dev-dependencies]`: add `rust_decimal_macros = "1"` if not already present (Task 12 may have added it).

`src/application/mod.rs` — add `pub mod subscription_matching;`.

- [ ] **Step 4: Run tests**

Run: `cargo test application::subscription_matching`
Expected: 2 tests PASS.

- [ ] **Step 5: Commit**

```bash
git add src/domain/transaction.rs src/infrastructure/transaction_repository.rs src/application/subscription_matching.rs src/application/mod.rs Cargo.toml Cargo.lock
git commit -m "feat(app): MatchChargesUseCase with ±5% amount, ±3d window"
```

> **Note on FX:** The spec calls for FX-converted matching when receipt and bank currencies differ. The v1 implementation above filters candidates by `currency` to keep the match fast and unambiguous. Multi-currency matching via `FxRateRepository::rate_as_of` is a follow-up; the trait already accepts `fx` so wiring it later is a one-method change.

---

## Task 22: Application — DetectLapsedUseCase

**Files:**
- Create: `src/application/subscription_lifecycle.rs`
- Modify: `src/application/mod.rs`

- [ ] **Step 1: Write the use case + test**

```rust
use std::sync::Arc;

use chrono::{Duration, Utc};

use crate::domain::subscription::{SubscriptionRepository, SubscriptionStatus};

pub struct DetectLapsedUseCase {
    pub subscriptions: Arc<dyn SubscriptionRepository>,
}

impl DetectLapsedUseCase {
    pub async fn run(&self) -> anyhow::Result<usize> {
        let threshold = Utc::now() - Duration::days(7);
        let lapsed = self.subscriptions.list_lapsed(threshold).await?;
        let count = lapsed.len();
        for sub in lapsed {
            self.subscriptions
                .update_after_charge(
                    sub.id,
                    sub.last_charged_at.unwrap_or(sub.started_at),
                    sub.next_expected_at.unwrap_or(sub.started_at),
                    SubscriptionStatus::Inactive,
                )
                .await?;
        }
        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::subscription::{
        BillingPeriod, Subscription, SubscriptionProvider,
    };
    use crate::infrastructure::subscription_repository::PgSubscriptionRepository;
    use crate::infrastructure::test_db;
    use rust_decimal_macros::dec;
    use uuid::Uuid;

    #[tokio::test]
    async fn marks_subs_past_threshold_inactive() {
        let pool = test_db::fresh_pool().await;
        let repo: Arc<dyn SubscriptionRepository> =
            Arc::new(PgSubscriptionRepository::new(pool));
        let user_id = Uuid::new_v4();
        let mut s = Subscription {
            id: Uuid::new_v4(),
            user_id,
            provider: SubscriptionProvider::Netflix,
            product_name: "Netflix".into(),
            merchant_key: "netflix.com:premium".into(),
            amount: dec!(15.99),
            currency: "USD".into(),
            billing_period: BillingPeriod::Monthly,
            status: SubscriptionStatus::Active,
            started_at: Utc::now() - Duration::days(60),
            last_charged_at: Some(Utc::now() - Duration::days(40)),
            next_expected_at: Some(Utc::now() - Duration::days(10)),
            category_id: None,
            created_at: Utc::now(),
        };
        repo.upsert_by_merchant_key(&s).await.unwrap();

        let uc = DetectLapsedUseCase { subscriptions: repo.clone() };
        let n = uc.run().await.unwrap();
        assert_eq!(n, 1);

        s = repo.find_by_id(s.id, user_id).await.unwrap().unwrap();
        assert_eq!(s.status, SubscriptionStatus::Inactive);
    }
}
```

- [ ] **Step 2: Register module**

Edit `src/application/mod.rs` — add `pub mod subscription_lifecycle;`.

- [ ] **Step 3: Run tests**

Run: `cargo test application::subscription_lifecycle`
Expected: 1 test PASSES.

- [ ] **Step 4: Commit**

```bash
git add src/application/subscription_lifecycle.rs src/application/mod.rs
git commit -m "feat(app): DetectLapsedUseCase"
```

---

## Task 23: Application — SubscriptionInventoryUseCase (forecast)

**Files:**
- Modify: `src/application/subscriptions.rs`

- [ ] **Step 1: Add inventory + forecast methods**

Add at the top of `src/application/subscriptions.rs` (alongside the existing imports):

```rust
use std::collections::HashMap;

use crate::domain::fx_rate::FxRateRepository;
use crate::domain::subscription::SubscriptionListFilter;
```

Then append, outside any `impl` block:

```rust
pub struct Forecast {
    pub base_currency: String,
    pub base_total: Decimal,
    pub by_currency: HashMap<String, Decimal>,
}

impl SubscriptionService {
    pub async fn list(
        &self,
        user_id: Uuid,
        status: Option<SubscriptionStatus>,
    ) -> anyhow::Result<Vec<Subscription>> {
        self.subscriptions
            .list_by_user(user_id, &SubscriptionListFilter { status })
            .await
    }

    pub async fn forecast_next_30d(
        &self,
        user_id: Uuid,
        base_currency: &str,
        fx: &dyn FxRateRepository,
    ) -> anyhow::Result<Forecast> {
        let subs = self
            .subscriptions
            .list_by_user(
                user_id,
                &SubscriptionListFilter { status: Some(SubscriptionStatus::Active) },
            )
            .await?;
        let mut by_currency: HashMap<String, Decimal> = HashMap::new();
        let mut base_total = Decimal::ZERO;
        let now = Utc::now();
        let thirty = Decimal::from(30);
        for s in subs {
            let cycle_days = Decimal::from(s.billing_period.cycle_days());
            let monthly_equivalent = s.amount * thirty / cycle_days;
            *by_currency.entry(s.currency.clone()).or_insert(Decimal::ZERO) += monthly_equivalent;
            let in_base = if s.currency == base_currency {
                monthly_equivalent
            } else {
                let rate = fx
                    .rate_as_of(&s.currency, base_currency, now)
                    .await?
                    .ok_or_else(|| anyhow::anyhow!("no FX rate {} → {}", s.currency, base_currency))?;
                monthly_equivalent * rate
            };
            base_total += in_base;
        }
        Ok(Forecast {
            base_currency: base_currency.to_string(),
            base_total,
            by_currency,
        })
    }
}
```

(Adjust `fx.rate_as_of(...)` signature to match the existing `FxRateRepository` API — verify the exact method name in `src/domain/fx_rate.rs`. If the method is named differently, use that.)

- [ ] **Step 2: Add a forecast test**

In the existing `mod tests` block:

```rust
#[tokio::test]
async fn forecast_sums_active_subs_normalized_to_monthly() {
    use crate::infrastructure::fx_rate_repository::PgFxRateRepository;
    let pool = test_db::fresh_pool().await;
    let user_id = Uuid::new_v4();
    let conns: Arc<dyn EmailConnectionRepository> =
        Arc::new(PgEmailConnectionRepository::new(pool.clone()));
    let subs: Arc<dyn SubscriptionRepository> =
        Arc::new(PgSubscriptionRepository::new(pool.clone()));
    let charges: Arc<dyn SubscriptionChargeRepository> =
        Arc::new(PgSubscriptionChargeRepository::new(pool.clone()));
    let fx: Arc<dyn FxRateRepository> = Arc::new(PgFxRateRepository::new(pool.clone()));
    let svc = SubscriptionService::new(
        conns,
        subs.clone(),
        charges,
        Arc::new(sync_tests::FakeFetcher { emails: vec![] }),
        Arc::new(ParserRegistry::default_set()),
    );

    use rust_decimal_macros::dec;
    subs.upsert_by_merchant_key(&Subscription {
        id: Uuid::new_v4(),
        user_id,
        provider: SubscriptionProvider::Netflix,
        product_name: "Netflix".into(),
        merchant_key: "netflix.com:premium".into(),
        amount: dec!(15.99),
        currency: "USD".into(),
        billing_period: BillingPeriod::Monthly,
        status: SubscriptionStatus::Active,
        started_at: Utc::now(),
        last_charged_at: None,
        next_expected_at: None,
        category_id: None,
        created_at: Utc::now(),
    })
    .await
    .unwrap();

    subs.upsert_by_merchant_key(&Subscription {
        id: Uuid::new_v4(),
        user_id,
        provider: SubscriptionProvider::AppleAppStore,
        product_name: "iCloud+".into(),
        merchant_key: "apps.apple.com:icloud_50gb".into(),
        amount: dec!(12.00),
        currency: "USD".into(),
        billing_period: BillingPeriod::Yearly,
        status: SubscriptionStatus::Active,
        started_at: Utc::now(),
        last_charged_at: None,
        next_expected_at: None,
        category_id: None,
        created_at: Utc::now(),
    })
    .await
    .unwrap();

    let f = svc.forecast_next_30d(user_id, "USD", &*fx).await.unwrap();
    // 15.99 + 12 * 30 / 365 ≈ 15.99 + 0.986 = ~16.98
    assert!(f.base_total > dec!(16.90));
    assert!(f.base_total < dec!(17.10));
}
```

- [ ] **Step 3: Run tests**

Run: `cargo test application::subscriptions::tests::forecast_sums`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add src/application/subscriptions.rs
git commit -m "feat(app): subscription inventory and 30-day forecast"
```

---

## Task 24: API DTOs

**Files:**
- Modify: `src/api/dto.rs`

- [ ] **Step 1: Append DTOs**

```rust
// ── Subscriptions ───────────────────────────────────────────────────────

#[derive(Debug, serde::Serialize)]
pub struct EmailConnectionResponse {
    pub id: Uuid,
    pub email_address: String,
    pub provider: String,
    pub status: String,
    pub last_synced_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, serde::Deserialize)]
pub struct GmailOAuthStartResponse {
    pub authorize_url: String,
    pub state: String,
}

#[derive(Debug, serde::Deserialize)]
pub struct GmailOAuthCallbackRequest {
    pub code: String,
    pub state: String,
}

#[derive(Debug, serde::Serialize)]
pub struct SubscriptionResponse {
    pub id: Uuid,
    pub provider: String,
    pub product_name: String,
    pub amount: Decimal,
    pub currency: String,
    pub billing_period: String,
    pub status: String,
    pub started_at: DateTime<Utc>,
    pub last_charged_at: Option<DateTime<Utc>>,
    pub next_expected_at: Option<DateTime<Utc>>,
    pub category_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, serde::Deserialize)]
pub struct SubscriptionListQuery {
    pub status: Option<String>, // active | inactive | all
}

#[derive(Debug, serde::Deserialize)]
pub struct UpdateSubscriptionRequest {
    pub product_name: Option<String>,
    pub billing_period: Option<String>,
    pub status: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_field")]
    pub category_id: Option<Option<Uuid>>,
}

#[derive(Debug, serde::Serialize)]
pub struct SubscriptionChargeResponse {
    pub id: Uuid,
    pub subscription_id: Uuid,
    pub amount: Decimal,
    pub currency: String,
    pub charged_at: DateTime<Utc>,
    pub kind: String,
    pub transaction_id: Option<Uuid>,
    pub match_status: String,
}

#[derive(Debug, serde::Deserialize)]
pub struct LinkChargeRequest {
    pub transaction_id: Uuid,
}

#[derive(Debug, serde::Serialize)]
pub struct ForecastResponse {
    pub base_currency: String,
    pub base_total: Decimal,
    pub by_currency: std::collections::HashMap<String, Decimal>,
}
```

- [ ] **Step 2: Build**

Run: `cargo build`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add src/api/dto.rs
git commit -m "feat(api): subscription DTOs"
```

---

## Task 25: API — email connection handlers

**Files:**
- Create: `src/api/handlers/email_connections.rs`
- Modify: `src/api/handlers/mod.rs`
- Modify: `src/api/routes.rs`
- Modify: `src/api/state.rs`

- [ ] **Step 1: Extend `AppState`**

In `src/api/state.rs`, add a field:

```rust
pub subscriptions: Arc<crate::application::subscriptions::SubscriptionService>,
pub matcher: Arc<crate::application::subscription_matching::MatchChargesUseCase>,
pub fx: Arc<dyn crate::domain::fx_rate::FxRateRepository>,
pub gmail_oauth: Arc<crate::infrastructure::email::oauth::OAuthConfig>,
```

Then add the supporting type in `src/infrastructure/email/oauth.rs`:

```rust
#[derive(Clone)]
pub struct OAuthConfig {
    pub client_id: String,
    pub client_secret: String,
    pub redirect_uri: String,
}
```

- [ ] **Step 2: Write handlers**

```rust
use axum::{
    Extension, Json,
    extract::{Path, State},
    http::StatusCode,
};
use uuid::Uuid;

use crate::api::{
    dto::{EmailConnectionResponse, GmailOAuthCallbackRequest},
    error::AppError,
    middleware::AuthUser,
    state::AppState,
};
use crate::application::subscriptions::ConnectGmailParams;
use crate::domain::email_connection::EmailConnection;

fn to_response(c: EmailConnection) -> EmailConnectionResponse {
    EmailConnectionResponse {
        id: c.id,
        email_address: c.email_address,
        provider: c.provider.as_str().to_string(),
        status: c.status.as_str().to_string(),
        last_synced_at: c.last_synced_at,
        created_at: c.created_at,
    }
}

pub async fn oauth_start(
    State(state): State<AppState>,
    Extension(AuthUser(user_id)): Extension<AuthUser>,
) -> Result<Json<serde_json::Value>, AppError> {
    let oauth_state = format!("{user_id}:{}", Uuid::new_v4());
    let url = format!(
        "https://accounts.google.com/o/oauth2/v2/auth?\
         client_id={}&redirect_uri={}&response_type=code&\
         scope=https%3A%2F%2Fwww.googleapis.com%2Fauth%2Fgmail.readonly%20\
         https%3A%2F%2Fwww.googleapis.com%2Fauth%2Fuserinfo.email&\
         access_type=offline&prompt=consent&state={}",
        urlencoding::encode(&state.gmail_oauth.client_id),
        urlencoding::encode(&state.gmail_oauth.redirect_uri),
        urlencoding::encode(&oauth_state),
    );
    Ok(Json(serde_json::json!({ "authorize_url": url, "state": oauth_state })))
}

pub async fn oauth_callback(
    State(state): State<AppState>,
    Extension(AuthUser(user_id)): Extension<AuthUser>,
    Json(req): Json<GmailOAuthCallbackRequest>,
) -> Result<(StatusCode, Json<EmailConnectionResponse>), AppError> {
    // Exchange code for tokens.
    let http = reqwest::Client::new();
    let token_resp: serde_json::Value = http
        .post("https://oauth2.googleapis.com/token")
        .form(&[
            ("client_id", state.gmail_oauth.client_id.as_str()),
            ("client_secret", state.gmail_oauth.client_secret.as_str()),
            ("code", req.code.as_str()),
            ("grant_type", "authorization_code"),
            ("redirect_uri", state.gmail_oauth.redirect_uri.as_str()),
        ])
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let access_token = token_resp["access_token"].as_str().unwrap_or("").to_string();
    let refresh_token = token_resp["refresh_token"].as_str().unwrap_or("").to_string();
    let expires_in = token_resp["expires_in"].as_i64().unwrap_or(3600);
    let expires_at = chrono::Utc::now() + chrono::Duration::seconds(expires_in - 60);

    // Get email address.
    let profile: serde_json::Value = http
        .get("https://www.googleapis.com/oauth2/v2/userinfo")
        .bearer_auth(&access_token)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let email_address = profile["email"].as_str().unwrap_or("unknown").to_string();

    let conn = state
        .subscriptions
        .connect_gmail(
            user_id,
            ConnectGmailParams {
                email_address,
                access_token,
                refresh_token,
                expires_at,
            },
        )
        .await?;
    Ok((StatusCode::CREATED, Json(to_response(conn))))
}

pub async fn list(
    State(state): State<AppState>,
    Extension(AuthUser(user_id)): Extension<AuthUser>,
) -> Result<Json<Vec<EmailConnectionResponse>>, AppError> {
    let conns = state.subscriptions.list_connections(user_id).await?;
    Ok(Json(conns.into_iter().map(to_response).collect()))
}

pub async fn resync(
    State(state): State<AppState>,
    Extension(AuthUser(user_id)): Extension<AuthUser>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, AppError> {
    // Verify the connection belongs to user.
    if state
        .subscriptions
        .connections
        .find_by_id(id, user_id)
        .await?
        .is_none()
    {
        return Err(crate::domain::subscription_error::SubscriptionError::ConnectionNotFound.into());
    }
    let new_ids = state.subscriptions.sync_connection(id).await?;
    if !new_ids.is_empty() {
        state.matcher.run_for_user(user_id).await?;
    }
    Ok(StatusCode::ACCEPTED)
}

pub async fn delete(
    State(state): State<AppState>,
    Extension(AuthUser(user_id)): Extension<AuthUser>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, AppError> {
    state.subscriptions.delete_connection(id, user_id).await?;
    Ok(StatusCode::NO_CONTENT)
}
```

Also map `SubscriptionError` in `api/error.rs` (extend the existing match): treat `ConnectionNotFound | SubscriptionNotFound | ChargeNotFound` as `404`, `DuplicateCharge` as `409`, the rest as `500`.

Add to `src/api/error.rs`:

```rust
use crate::domain::subscription_error::SubscriptionError;
// inside IntoResponse for AppError, after the DomainError branch:
if let Some(s) = err.downcast_ref::<SubscriptionError>() {
    let (status, msg) = match s {
        SubscriptionError::ConnectionNotFound
        | SubscriptionError::SubscriptionNotFound
        | SubscriptionError::ChargeNotFound => (StatusCode::NOT_FOUND, s.to_string()),
        SubscriptionError::DuplicateCharge(_) => (StatusCode::CONFLICT, s.to_string()),
        _ => (StatusCode::INTERNAL_SERVER_ERROR, s.to_string()),
    };
    return (status, Json(json!({"error": msg}))).into_response();
}
```

- [ ] **Step 3: Add routes**

In `src/api/routes.rs`, add to the `protected` chain:

```rust
.route(
    "/me/email-connections/gmail/oauth/start",
    post(email_connections::oauth_start),
)
.route(
    "/me/email-connections/gmail/oauth/callback",
    post(email_connections::oauth_callback),
)
.route(
    "/me/email-connections",
    get(email_connections::list),
)
.route(
    "/me/email-connections/{id}",
    delete(email_connections::delete),
)
.route(
    "/me/email-connections/{id}/resync",
    post(email_connections::resync),
)
```

And add `email_connections` to the `use crate::api::handlers::{...}` import line.

Edit `src/api/handlers/mod.rs` — add `pub mod email_connections;`.

- [ ] **Step 4: Build**

Run: `cargo build`
Expected: PASS (this requires `main.rs` wiring done in Task 28 — but library code should compile because `AppState` now requires the new fields; you'll need a temporary `todo!()` in `main.rs` or do this build only after Task 28. Easier: defer the `cargo build` check until Task 28).

- [ ] **Step 5: Commit**

```bash
git add src/api/handlers/email_connections.rs src/api/handlers/mod.rs src/api/routes.rs src/api/state.rs src/api/error.rs src/infrastructure/email/oauth.rs
git commit -m "feat(api): email-connection handlers and routes"
```

---

## Task 26: API — subscription handlers

**Files:**
- Create: `src/api/handlers/subscriptions.rs`
- Modify: `src/api/handlers/mod.rs`
- Modify: `src/api/routes.rs`

- [ ] **Step 1: Write handlers**

```rust
use axum::{
    Extension, Json,
    extract::{Path, Query, State},
    http::StatusCode,
};
use uuid::Uuid;

use crate::api::{
    dto::{
        ForecastResponse, LinkChargeRequest, SubscriptionChargeResponse,
        SubscriptionListQuery, SubscriptionResponse, UpdateSubscriptionRequest,
    },
    error::AppError,
    middleware::AuthUser,
    state::AppState,
};
use crate::domain::error::DomainError;
use crate::domain::subscription::{BillingPeriod, Subscription, SubscriptionStatus};
use crate::domain::subscription_charge::SubscriptionCharge;
use crate::domain::subscription_error::SubscriptionError;

fn to_resp(s: Subscription) -> SubscriptionResponse {
    SubscriptionResponse {
        id: s.id,
        provider: s.provider.as_str().to_string(),
        product_name: s.product_name,
        amount: s.amount,
        currency: s.currency,
        billing_period: s.billing_period.as_str().to_string(),
        status: s.status.as_str().to_string(),
        started_at: s.started_at,
        last_charged_at: s.last_charged_at,
        next_expected_at: s.next_expected_at,
        category_id: s.category_id,
        created_at: s.created_at,
    }
}

fn charge_resp(c: SubscriptionCharge) -> SubscriptionChargeResponse {
    SubscriptionChargeResponse {
        id: c.id,
        subscription_id: c.subscription_id,
        amount: c.amount,
        currency: c.currency,
        charged_at: c.charged_at,
        kind: c.kind.as_str().to_string(),
        transaction_id: c.transaction_id,
        match_status: c.match_status.as_str().to_string(),
    }
}

pub async fn list(
    State(state): State<AppState>,
    Extension(AuthUser(user_id)): Extension<AuthUser>,
    Query(q): Query<SubscriptionListQuery>,
) -> Result<Json<Vec<SubscriptionResponse>>, AppError> {
    let status = match q.status.as_deref() {
        Some("active") => Some(SubscriptionStatus::Active),
        Some("inactive") => Some(SubscriptionStatus::Inactive),
        Some("all") | None => None,
        Some(other) => {
            return Err(DomainError::InvalidInput(format!("unknown status: {other}")).into())
        }
    };
    let items = state.subscriptions.list(user_id, status).await?;
    Ok(Json(items.into_iter().map(to_resp).collect()))
}

pub async fn get(
    State(state): State<AppState>,
    Extension(AuthUser(user_id)): Extension<AuthUser>,
    Path(id): Path<Uuid>,
) -> Result<Json<SubscriptionResponse>, AppError> {
    let s = state
        .subscriptions
        .subscriptions
        .find_by_id(id, user_id)
        .await?
        .ok_or(SubscriptionError::SubscriptionNotFound)?;
    Ok(Json(to_resp(s)))
}

pub async fn patch(
    State(state): State<AppState>,
    Extension(AuthUser(user_id)): Extension<AuthUser>,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdateSubscriptionRequest>,
) -> Result<Json<SubscriptionResponse>, AppError> {
    let billing_period = req
        .billing_period
        .as_deref()
        .map(BillingPeriod::from_str)
        .transpose()
        .map_err(|e| DomainError::InvalidInput(e.to_string()))?;
    let status = req
        .status
        .as_deref()
        .map(SubscriptionStatus::from_str)
        .transpose()
        .map_err(|e| DomainError::InvalidInput(e.to_string()))?;
    state
        .subscriptions
        .subscriptions
        .update_editable_fields(
            id,
            user_id,
            req.product_name,
            req.category_id,
            billing_period,
            status,
        )
        .await?;
    let s = state
        .subscriptions
        .subscriptions
        .find_by_id(id, user_id)
        .await?
        .ok_or(SubscriptionError::SubscriptionNotFound)?;
    Ok(Json(to_resp(s)))
}

pub async fn delete(
    State(state): State<AppState>,
    Extension(AuthUser(user_id)): Extension<AuthUser>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, AppError> {
    state
        .subscriptions
        .subscriptions
        .delete(id, user_id)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn list_charges(
    State(state): State<AppState>,
    Extension(AuthUser(user_id)): Extension<AuthUser>,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<SubscriptionChargeResponse>>, AppError> {
    let items = state
        .subscriptions
        .charges
        .list_for_subscription(id, user_id)
        .await?;
    Ok(Json(items.into_iter().map(charge_resp).collect()))
}

pub async fn forecast(
    State(state): State<AppState>,
    Extension(AuthUser(user_id)): Extension<AuthUser>,
) -> Result<Json<ForecastResponse>, AppError> {
    let settings = state.user_settings.get(user_id).await?;
    let f = state
        .subscriptions
        .forecast_next_30d(user_id, &settings.base_currency, &*state.fx)
        .await?;
    Ok(Json(ForecastResponse {
        base_currency: f.base_currency,
        base_total: f.base_total,
        by_currency: f.by_currency,
    }))
}

pub async fn link_charge(
    State(state): State<AppState>,
    Extension(AuthUser(user_id)): Extension<AuthUser>,
    Path(id): Path<Uuid>,
    Json(req): Json<LinkChargeRequest>,
) -> Result<StatusCode, AppError> {
    let charge = state
        .subscriptions
        .charges
        .find_by_id(id, user_id)
        .await?
        .ok_or(SubscriptionError::ChargeNotFound)?;
    // Ensure tx belongs to the user (find_by_id checks user_id).
    state
        .transactions
        .get(req.transaction_id, user_id)
        .await?;
    state
        .subscriptions
        .charges
        .update_match(
            charge.id,
            Some(req.transaction_id),
            crate::domain::subscription_charge::ChargeMatchStatus::Matched,
        )
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn unlink_charge(
    State(state): State<AppState>,
    Extension(AuthUser(user_id)): Extension<AuthUser>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, AppError> {
    let charge = state
        .subscriptions
        .charges
        .find_by_id(id, user_id)
        .await?
        .ok_or(SubscriptionError::ChargeNotFound)?;
    state
        .subscriptions
        .charges
        .update_match(
            charge.id,
            None,
            crate::domain::subscription_charge::ChargeMatchStatus::Pending,
        )
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
```

(Adjust `state.transactions.get(...)` to match the actual `TransactionService` method name in `src/application/transactions.rs`. If it's `get_transaction` or similar, use that.)

- [ ] **Step 2: Add routes**

In `src/api/routes.rs`:

```rust
.route("/subscriptions", get(subscriptions::list))
.route(
    "/subscriptions/{id}",
    get(subscriptions::get).patch(subscriptions::patch).delete(subscriptions::delete),
)
.route("/subscriptions/{id}/charges", get(subscriptions::list_charges))
.route("/subscriptions/forecast", get(subscriptions::forecast))
.route(
    "/subscription-charges/{id}/link",
    post(subscriptions::link_charge),
)
.route(
    "/subscription-charges/{id}/unlink",
    post(subscriptions::unlink_charge),
)
```

Edit `src/api/handlers/mod.rs` — add `pub mod subscriptions;`.

- [ ] **Step 3: Commit (build verified in Task 28 after wiring)**

```bash
git add src/api/handlers/subscriptions.rs src/api/handlers/mod.rs src/api/routes.rs
git commit -m "feat(api): subscription handlers and routes"
```

---

## Task 27: Wire MatchChargesUseCase into Monobank sync

**Files:**
- Modify: `src/application/monobank.rs`

The matcher must fire at the end of each monobank sync, so receipts that arrived before the bank transaction get matched on the next sync.

- [ ] **Step 1: Inspect `MonobankService`**

Read `src/application/monobank.rs`. Identify the sync method (likely `resync_window`, `run_sync`, or similar) where transactions are persisted.

- [ ] **Step 2: Add an optional matcher dependency**

Add a field on `MonobankService`:

```rust
pub matcher: Option<Arc<crate::application::subscription_matching::MatchChargesUseCase>>,
```

Update its constructor `new(...)` to take a `matcher: Option<...>` and pass `None` from tests that don't need it, `Some(matcher)` from `main.rs`.

At the end of each sync method (after transactions are persisted), call:

```rust
if let Some(m) = &self.matcher {
    if let Err(e) = m.run_for_user(user_id).await {
        tracing::warn!("matcher failed for user {user_id}: {e:?}");
    }
}
```

- [ ] **Step 3: Update existing call sites**

`main.rs`, all `tests/api/*.rs` files, and any other place that instantiates `MonobankService::new(...)` — add the new argument (pass `None` from tests).

Run: `cargo build`
Expected: PASS once all call sites are updated.

- [ ] **Step 4: Commit**

```bash
git add src/application/monobank.rs src/main.rs tests/
git commit -m "feat(app): trigger subscription matcher after monobank sync"
```

---

## Task 28: main.rs — wire DI + hourly scheduler

**Files:**
- Modify: `src/main.rs`

- [ ] **Step 1: Add new use imports**

```rust
use std::time::Duration;

use moneykeeper::application::subscription_lifecycle::DetectLapsedUseCase;
use moneykeeper::application::subscription_matching::MatchChargesUseCase;
use moneykeeper::application::subscriptions::SubscriptionService;
use moneykeeper::domain::email_connection::EmailConnectionRepository;
use moneykeeper::domain::subscription::SubscriptionRepository;
use moneykeeper::domain::subscription_charge::SubscriptionChargeRepository;
use moneykeeper::infrastructure::email::gmail_client::GmailClient;
use moneykeeper::infrastructure::email::oauth::OAuthConfig;
use moneykeeper::infrastructure::email::parsers::ParserRegistry;
use moneykeeper::infrastructure::email_connection_repository::PgEmailConnectionRepository;
use moneykeeper::infrastructure::subscription_charge_repository::PgSubscriptionChargeRepository;
use moneykeeper::infrastructure::subscription_repository::PgSubscriptionRepository;
```

- [ ] **Step 2: Read env config**

After the existing env-var reads, add:

```rust
let gmail_client_id =
    std::env::var("GMAIL_CLIENT_ID").expect("GMAIL_CLIENT_ID must be set");
let gmail_client_secret =
    std::env::var("GMAIL_CLIENT_SECRET").expect("GMAIL_CLIENT_SECRET must be set");
let gmail_redirect_uri = std::env::var("GMAIL_REDIRECT_URI")
    .unwrap_or_else(|_| format!("{public_url}/me/email-connections/gmail/oauth/callback"));
```

- [ ] **Step 3: Build the new service graph**

Right before constructing `AppState`:

```rust
let email_conn_repo: Arc<dyn EmailConnectionRepository> =
    Arc::new(PgEmailConnectionRepository::new(pool.clone()));
let subscription_repo: Arc<dyn SubscriptionRepository> =
    Arc::new(PgSubscriptionRepository::new(pool.clone()));
let charge_repo: Arc<dyn SubscriptionChargeRepository> =
    Arc::new(PgSubscriptionChargeRepository::new(pool.clone()));
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
```

- [ ] **Step 4: Pass matcher to monobank service**

Update the `MonobankService::new(...)` call to include `Some(matcher.clone())`.

- [ ] **Step 5: Extend AppState construction**

```rust
let state = AppState {
    accounts: ...,
    transactions: ...,
    categories: ...,
    monobank: monobank_service.clone(),
    user_settings: Arc::clone(&user_settings_service),
    supabase_jwks: Arc::new(jwks),
    subscriptions: Arc::clone(&subscription_service),
    matcher: Arc::clone(&matcher),
    fx: Arc::clone(&fx_repo),
    gmail_oauth: oauth_config,
};
```

- [ ] **Step 6: Spawn the hourly scheduler**

Before `axum::serve(...)`:

```rust
{
    let subs = Arc::clone(&subscription_service);
    let matcher = Arc::clone(&matcher);
    let conns = Arc::clone(&email_conn_repo);
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(3600));
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

{
    let lifecycle = Arc::clone(&lifecycle);
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(86_400));
        loop {
            ticker.tick().await;
            if let Err(e) = lifecycle.run().await {
                tracing::warn!("lapse-detection failed: {e:?}");
            }
        }
    });
}
```

- [ ] **Step 7: Build and run the whole suite**

Run: `cargo build`
Expected: PASS.

Run: `cargo test`
Expected: PASS (all unit + integration tests).

- [ ] **Step 8: Commit**

```bash
git add src/main.rs
git commit -m "feat: wire subscription services and hourly Gmail sync"
```

---

## Task 29: Integration test — end-to-end roundtrip

**Files:**
- Create: `tests/api/subscriptions.rs`
- Modify: `tests/api/main.rs` (or wherever the test crate's module list lives)
- Modify: `tests/api/helpers.rs` if needed to supply a fake `EmailFetcher`/`OAuthConfig` to the test app

The aim: drive a complete flow through the running axum app — ingest a fake email → assert subscription + charge present → create a monobank-like expense → run matcher → assert charge is `Matched` and tx is categorized.

- [ ] **Step 1: Extend `tests/api/helpers.rs`**

The helper currently builds an `AppState` for tests using real repos with a fresh pool. Update it (or add a sibling helper) that accepts an injected `EmailFetcher` and constructs `SubscriptionService` with it. For tasks where the test doesn't care about Gmail, default to a `FakeFetcher` that returns no emails.

(The exact shape depends on `helpers.rs` — see `tests/api/monobank.rs` for the existing pattern of wiring services.)

- [ ] **Step 2: Write the test**

Sketch:

```rust
use moneykeeper::application::subscriptions::ConnectGmailParams;
use rust_decimal_macros::dec;

#[tokio::test]
async fn ingest_receipt_then_match_via_monobank_tx() {
    // 1. boot app with fake fetcher returning one Netflix renewal
    let (app, ctx) = helpers::boot_with_fake_emails(vec![
        helpers::netflix_renewal_email("<msg-1>", dec!(15.99)),
    ]).await;

    // 2. connect-gmail shortcut via service (skip OAuth)
    let conn = ctx.subscriptions.connect_gmail(ctx.user_id, ConnectGmailParams {
        email_address: "x@y.com".into(),
        access_token: "a".into(),
        refresh_token: "r".into(),
        expires_at: chrono::Utc::now() + chrono::Duration::hours(1),
    }).await.unwrap();

    // 3. trigger sync via API
    let resp = app
        .post(&format!("/me/email-connections/{}/resync", conn.id))
        .add_header("authorization", &ctx.bearer)
        .await;
    assert_eq!(resp.status_code(), 202);

    // 4. subscription created
    let list = app
        .get("/subscriptions")
        .add_header("authorization", &ctx.bearer)
        .await
        .json::<serde_json::Value>();
    assert_eq!(list.as_array().unwrap().len(), 1);

    // 5. simulate monobank tx
    let tx = helpers::insert_expense(&ctx, dec!(16.00), "USD", chrono::Utc::now()).await;

    // 6. run matcher
    ctx.matcher.run_for_user(ctx.user_id).await.unwrap();

    // 7. assert charge Matched
    let charges = app
        .get(&format!("/subscriptions/{}/charges", /* sub id */ list[0]["id"].as_str().unwrap()))
        .add_header("authorization", &ctx.bearer)
        .await
        .json::<serde_json::Value>();
    let charge = &charges[0];
    assert_eq!(charge["match_status"], "Matched");
    assert_eq!(charge["transaction_id"], serde_json::json!(tx.id));
}
```

The exact helpers `boot_with_fake_emails`, `netflix_renewal_email`, `insert_expense`, and `ctx` fields you'll need to add to `helpers.rs`. Mirror what `tests/api/monobank.rs` already does for monobank-backed flows.

- [ ] **Step 3: Run the test**

Run: `cargo test --test api subscriptions::ingest_receipt_then_match`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add tests/api
git commit -m "test(api): end-to-end subscription ingestion + matching"
```

---

## Task 30: Final verification

- [ ] **Step 1: Full build**

Run: `cargo build`
Expected: PASS, no warnings.

- [ ] **Step 2: Full test run**

Run: `cargo test`
Expected: ALL PASS.

- [ ] **Step 3: Clippy**

Run: `cargo clippy --all-targets -- -D warnings`
Expected: PASS, no warnings.

- [ ] **Step 4: Format**

Run: `cargo fmt --check`
Expected: PASS.

- [ ] **Step 5: Final commit if anything was reformatted**

```bash
git add -A
git commit -m "chore: fmt + clippy cleanup" || true
```

---

## Out-of-scope follow-ups (do NOT implement in this plan)

- Token encryption at rest (extend to both `BankConnection.token` and `EmailConnection.oauth_*`).
- Multi-currency matcher path (use `FxRateRepository::rate_as_of` to convert before bounding).
- Spotify + additional parsers.
- Gmail history API for true incremental sync (today we use `newer_than:30d` queries).
- Bank-pattern detection as a second `ParsedReceipt` source.
- Multi-tenant CASA assessment for Gmail restricted scope.
- Parsing cancellation/refund emails into subscription status transitions.
