# Subscriptions — Design

**Date:** 2026-05-26
**Status:** Design

## Goal

Add first-class handling of recurring service subscriptions (Google Play, Apple App Store, Netflix, …) to moneykeeper so that:

1. **Inventory & forecast** — the user sees a list of active subscriptions and projected monthly/yearly spend in their base currency.
2. **Auto-categorize transactions** — when a monobank transaction corresponds to a known subscription charge, it is automatically tagged with the subscription's category.
3. **Detect new subscriptions** — new subscriptions are discovered automatically from email receipts.

## Source decisions (already settled in brainstorm)

- **Audience:** single user now, multi-tenant later — design is source-pluggable (no Gmail-specific code below the infrastructure layer).
- **First detection source:** Gmail receipts. There is no public Google Play API for end users to query their own purchase history; the Play Developer API is for app publishers. Bank-pattern detection is a viable follow-up source but out of scope here.
- **First providers:** Google Play, Apple App Store, Netflix. Spotify and others are follow-up specs.
- **Inbox access:** Gmail API + hourly polling (no Pub/Sub).
- **Source of truth for money flow:** the **bank transaction** remains source of truth. Email receipts become `SubscriptionCharge` records that *link* to existing `Transaction`s; they do not create transactions of their own. This avoids duplicate transactions while still enabling auto-categorization.
- **Subscription scope:** user-scoped (not account-scoped) — survives card changes.
- **Matching:** amount within ±5% (FX-converted) and time within ±3 days; unmatched charges stay `Pending` and retry after each monobank sync.
- **Lifecycle:** lapse-detection only in v1 (>7 days past expected charge → `Inactive`). No parsing of cancellation/refund emails into lifecycle changes.
- **Parser strategy:** trait-based per-provider parsers; start with three and add more incrementally.

## Architecture overview

Follows existing DDD layout. New domain aggregates and traits in `src/domain/`, application use cases in `src/application/`, Gmail client and per-provider parsers in `src/infrastructure/`, HTTP handlers under `src/api/handlers/`.

```
domain/
  email_connection.rs       # EmailConnection aggregate + repo trait
  email.rs                  # RawEmail + EmailFetcher trait
  receipt_parser.rs         # ParsedReceipt + ReceiptParser trait
  subscription.rs           # Subscription + SubscriptionCharge + repo traits
application/
  subscriptions.rs          # ConnectGmailUseCase, SyncEmailUseCase, SubscriptionInventoryUseCase
  subscription_matching.rs  # MatchChargesUseCase, DetectLapsedUseCase
infrastructure/
  email/
    gmail_client.rs         # impl EmailFetcher
    parsers/
      mod.rs                # ParserRegistry
      google_play.rs
      apple.rs
      netflix.rs
  email_connection_repository.rs
  subscription_repository.rs
  subscription_charge_repository.rs
api/handlers/
  email_connections.rs
  subscriptions.rs
```

## Domain model

### `EmailConnection` — parallels `BankConnection`

```rust
pub enum EmailProvider { Gmail }

pub enum EmailConnectionStatus { Pending, Connected, Failed }

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
    pub last_history_id: Option<String>, // Gmail incremental cursor
    pub created_at: DateTime<Utc>,
}
```

`EmailConnectionRepository` trait mirrors `BankConnectionRepository`: `create`, `find_by_id`, `list_by_user`, `update_status`, `update_tokens`, `delete`.

Tokens are stored as plaintext in v1, matching how `BankConnection.token` is currently handled. Encryption-at-rest is a known follow-up that should be tackled across both connection types together.

### `Subscription`

```rust
pub enum SubscriptionProvider { GooglePlay, AppleAppStore, Netflix, Other }

pub enum BillingPeriod { Weekly, Monthly, Yearly }

pub enum SubscriptionStatus { Active, Inactive }

pub struct Subscription {
    pub id: Uuid,
    pub user_id: Uuid,
    pub provider: SubscriptionProvider,
    pub product_name: String,    // "Netflix Premium"
    pub merchant_key: String,    // canonical dedup key, e.g. "netflix.com:premium"
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
```

Uniqueness: `(user_id, merchant_key)`. Upsert on ingestion.

### `SubscriptionCharge`

```rust
pub enum ChargeMatchStatus { Pending, Matched, Unmatched }

pub enum ReceiptKind { NewSubscription, Renewal, OneTimePurchase, Refund }

pub struct SubscriptionCharge {
    pub id: Uuid,
    pub subscription_id: Uuid,
    pub user_id: Uuid,
    pub amount: Decimal,
    pub currency: String,
    pub charged_at: DateTime<Utc>,         // from receipt
    pub email_message_id: String,           // RFC 822 Message-ID; UNIQUE — idempotent
    pub kind: ReceiptKind,
    pub transaction_id: Option<Uuid>,       // FK to transactions; null until matched
    pub match_status: ChargeMatchStatus,
    pub created_at: DateTime<Utc>,
}
```

`SubscriptionChargeRepository`: `create_idempotent` (insert-or-ignore on `email_message_id`), `find_pending_for_user`, `update_match`, `list_for_subscription`.

### `RawEmail` + `EmailFetcher`

```rust
pub struct RawEmail {
    pub message_id: String,
    pub from: String,
    pub subject: String,
    pub received_at: DateTime<Utc>,
    pub body_text: Option<String>,
    pub body_html: Option<String>,
}

#[async_trait]
pub trait EmailFetcher: Send + Sync {
    /// Returns new emails since the connection's cursor, plus a new cursor to persist.
    async fn fetch_new(
        &self, conn: &EmailConnection,
    ) -> anyhow::Result<(Vec<RawEmail>, String /* new cursor */)>;
}
```

### `ParsedReceipt` + `ReceiptParser`

```rust
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

## Persistence

Three new migrations:

- `0009_email_connections.sql` — columns mirroring `EmailConnection`, indexes on `(user_id)` and `(status)`.
- `0010_subscriptions.sql` — columns mirroring `Subscription`. Unique index on `(user_id, merchant_key)`. Index on `(user_id, status)`.
- `0011_subscription_charges.sql` — columns mirroring `SubscriptionCharge`. Unique index on `email_message_id`. Indexes on `(user_id, charged_at)` and `(transaction_id)`. FK `subscription_id → subscriptions(id) ON DELETE CASCADE` (deleting a subscription removes its charges). FK `transaction_id → transactions(id) ON DELETE SET NULL` (deleting a transaction leaves the charge but unlinks it).

## Ingestion pipeline

**Gmail client (`infrastructure/email/gmail_client.rs`)**
- Uses `oauth2` and `reqwest` crates against `https://gmail.googleapis.com`.
- Refreshes the access token via the refresh token when `access_token_expires_at` is in the past; persists new tokens through `EmailConnectionRepository::update_tokens` before continuing.
- First run for a connection: `users.messages.list` with `q=from:(googleplay-noreply@google.com OR info@account.netflix.com OR no_reply@email.apple.com) newer_than:30d`. Fetch full messages, return `RawEmail`s + the latest `historyId` as cursor.
- Subsequent runs: `users.history.list` from the stored `historyId`; if Gmail returns 404 (historyId expired), fall back to the date-bounded query using `last_synced_at`.

**Parser registry (`infrastructure/email/parsers/mod.rs`)**
- Holds `Vec<Box<dyn ReceiptParser>>`. `fn find(&self, from: &str) -> Option<&dyn ReceiptParser>` returns the first parser whose `matches_sender` returns true.
- Three concrete parsers (`google_play.rs`, `apple.rs`, `netflix.rs`). Each uses the `scraper` crate for HTML body and `regex` for amount/currency extraction. Per-provider fixture emails (redacted) live in `tests/fixtures/receipts/<provider>/`.

**`SyncEmailUseCase`** (per connection):
1. Load connection; refresh OAuth token if expired (persist new tokens).
2. `fetcher.fetch_new(&conn)` → `(emails, new_cursor)`.
3. For each `RawEmail`:
   - `registry.find(&email.from)` → parser; if none, skip.
   - `parser.parse(email)` → `Option<ParsedReceipt>`; if `None`, skip (non-receipt email from same sender).
   - Upsert `Subscription` by `(user_id, merchant_key)`; if newly created, use `kind == NewSubscription` to set `started_at = charged_at`, otherwise leave existing `started_at` intact and just refresh `amount`/`currency`/`billing_period` if the receipt disagrees with stored values.
   - `SubscriptionChargeRepository::create_idempotent(...)` with `email_message_id` as the dedup key. Returns the charge id if newly inserted, `None` if it already existed.
4. Persist `last_history_id = new_cursor` and `last_synced_at = now`.
5. Return the list of newly-inserted charge ids → handed to `MatchChargesUseCase`.

**Scheduling**
- Hourly job wired in `main.rs` alongside `FxSyncUseCase`. Iterates `email_connections` with `status = Connected`. Per-connection failures bump `status` to `Failed`, log, and continue — one user's bad token doesn't block others.
- `DetectLapsedUseCase` runs daily on the same scheduler.

**Refunds (v1)**: Refund receipts are parsed and persisted as `SubscriptionCharge` rows with negative `amount` and `kind = Refund`. They are **not** matched against bank refund transactions in v1.

## Matching

**`MatchChargesUseCase::run_for_user(user_id)`** — idempotent, safe to call repeatedly:

For each charge with `match_status = Pending`:

1. **FX-convert** the charge amount into each candidate transaction's currency using `FxRateRepository::rate_as_of(charged_at)`. (The repo already supports as-of lookups.)
2. **Candidate query** — `transactions` for this user with:
   - `kind = Expense`
   - `transacted_at` within `charged_at ± 3 days`
   - `amount` within `±5%` of FX-converted value (DB-side bounded range; final exact-percentage check in Rust)
   - Not already linked to another charge (left-join `subscription_charges`)
3. **Score** each candidate by `score = abs_amount_delta_pct + (abs_time_delta_hours / 24.0)`. Pick the lowest score.
4. **On match**: set `charge.transaction_id`, `match_status = Matched`. If the linked transaction's `category_id` is `NULL`, write the subscription's `category_id` onto it. Recompute the subscription's `last_charged_at` and `next_expected_at` (`last_charged_at + billing_period`). If the subscription was `Inactive`, flip to `Active`.
5. **On no match**: leave as `Pending`. After 7 days with no match, transition `Pending → Unmatched` (surfaced in UI; no automatic action).

**Trigger points**:
- At the end of `SyncEmailUseCase` (immediate retry for charges just ingested).
- At the end of `MonobankSyncUseCase` (the common case — receipt arrives before the bank tx).

## Lifecycle

**`DetectLapsedUseCase`** (daily): for each `Subscription` with `status = Active` and `next_expected_at < now - 7 days`, set `status = Inactive`. Reactivation happens automatically inside `MatchChargesUseCase` when a new charge gets matched.

Manual transitions (cancel / reactivate) are available via `PATCH /subscriptions/{id}` and override automatic state.

## API surface

OAuth & connections:

| Method | Path | Notes |
|---|---|---|
| `POST` | `/me/email-connections/gmail/oauth/start` | Returns `{ authorize_url, state }` |
| `POST` | `/me/email-connections/gmail/oauth/callback` | Body `{ code, state }` → exchanges code, stores connection |
| `GET`  | `/me/email-connections` | List connections for current user |
| `POST` | `/me/email-connections/{id}/resync` | Manual sync (mirrors monobank resync at commit `6c3cfae`) |
| `DELETE` | `/me/email-connections/{id}` | |

Subscriptions:

| Method | Path | Notes |
|---|---|---|
| `GET`  | `/subscriptions?status=active\|inactive\|all` | Inventory |
| `GET`  | `/subscriptions/{id}` | Detail incl. recent charges |
| `PATCH` | `/subscriptions/{id}` | Editable: `product_name`, `category_id`, `billing_period`, `status` |
| `DELETE` | `/subscriptions/{id}` | Cascades to `subscription_charges` (charges' `transaction_id` is `ON DELETE SET NULL` on the tx side; deleting the sub deletes its charges) |
| `GET`  | `/subscriptions/forecast` | Next-30-day projection in user's `base_currency` from `user_settings` |

Charges:

| Method | Path | Notes |
|---|---|---|
| `GET`  | `/subscriptions/{id}/charges` | |
| `POST` | `/subscription-charges/{id}/link` | Body `{ transaction_id }` — manual override |
| `POST` | `/subscription-charges/{id}/unlink` | |

## Error handling

`SubscriptionError` via `thiserror`:

- `ParserNotFound`
- `OAuthRefreshFailed`
- `DuplicateCharge`
- `MatchAmbiguous`
- `ConnectionNotFound`
- `SubscriptionNotFound`

Propagated to HTTP through the existing `api/error.rs` mapping.

## Testing

**Unit**
- Each parser: NewSubscription + Renewal fixtures per provider (at minimum), plus one non-receipt false-positive case.
- `MatchChargesUseCase`: FX scenarios (UAH bank tx vs USD/EUR receipt), edge cases at ±5% and ±3-day boundaries, ambiguous candidates.
- `DetectLapsedUseCase`: monthly sub past due, yearly sub within window, recently reactivated.

**Integration (`tests/api/`)**
- `subscriptions.rs` — connect with mocked OAuth exchange and a fake `EmailFetcher`; sync, list, forecast, manual link/unlink, delete.
- Use-case roundtrip: fake fetcher returns canned `RawEmail`s → assert subscription + charge persisted; second run is a no-op (idempotency via `email_message_id`).
- Cross-feature: ingest receipt → run monobank sync that produces a matching tx → assert charge becomes `Matched` and tx is categorized.

**Gated**
- `gmail_client` integration test behind an env var, exercising a real Gmail test account.

## Out of scope for v1

- **Token encryption at rest** — matches current `BankConnection.token` handling; follow-up across both connection types.
- **Gmail push notifications (Pub/Sub)** — overkill; receipts aren't time-sensitive.
- **Parsing cancellation/refund emails into lifecycle transitions** — refunds stored, not applied to subscription status.
- **Apple/Google in-app trial → paid transition detection** — first paid charge starts the subscription.
- **Multi-tenant Gmail restricted-scope verification (CASA)** — design is multi-tenant-ready; the policy work happens when we actually open up.
- **Spotify and other providers** — separate follow-up specs, each adding one `ReceiptParser` impl + fixtures.
- **Bank-pattern detection as a source** — separate follow-up spec; will plug in as a second source of `ParsedReceipt`-equivalent events.
