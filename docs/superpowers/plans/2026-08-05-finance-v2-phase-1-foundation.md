# Finance V2 Phase 1 — Foundation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

> **Implementation status (2026-08-13):** All functional steps and exit criteria are verified. Commit-boundary boxes remain open because this implementation is being handed off without altering the caller's existing staged/dirty Git state.

**Goal:** Establish the Finance V2 shared kernel, every planned bounded-context skeleton, reference/classification/preferences foundations, concrete outbox/inbox/process-manager runtime, and a brand-new SQLx migration lineage that can only run against an empty or already-marked Finance V2 PostgreSQL database.

**Architecture:** Finance V2 is a context-first DDD modular monolith. Contexts own their domain, application, infrastructure, API, and tables. They collaborate only through typed public facades, a transactional outbox, idempotent inbox consumers, and durable process managers with fenced leases. The shared kernel is deliberately small: universal typed identifiers plus an ID-newtype macro, `Money`, `CurrencyCode`, `IdempotencyKey`, `Clock`, and versioned event-envelope metadata. Aggregate IDs such as LedgerAccountId or ProviderConnectionId are declared by their owning contexts and cross a boundary only through that context's public contract. Phase 1 builds and tests V2 alongside the still-runnable legacy application; it does **not** switch `main.rs`, the default test migrator, Docker state, public routes, or `DATABASE_URL`. The irreversible blank-database/runtime/API cutover is Phase 8.

**Tech Stack:** Rust 2024, PostgreSQL 16, sqlx 0.8, axum 0.8, tokio, rust_decimal, uuid, chrono, serde, thiserror, testcontainers

**Dependencies:** None. This phase is the prerequisite for Phase 2 (Ledger) and Phase 3 (Banking).

---

## Non-negotiable decisions

- Finance V2 starts from a blank database. There is no legacy finance-data copy, backfill, compatibility view, dual write, or reconciliation task.
- Existing files under `src/infrastructure/migrations/` are immutable historical artifacts. Do not edit them, renumber them, or append Finance V2 as `0026`.
- Finance V2 migrations live under `src/infrastructure/migrations_v2/` and begin at `0001`.
- Freeze the collision-free implementation-order lineage now: `0001_shared_reference_preferences.sql`, `0002_integration_runtime.sql`, `0003_ledger.sql`, `0004_banking.sql`, `0005_mail.sql`, `0006_recurring.sql`, `0007_reference_fx.sql`, `0008_reporting.sql`, `0009_sharing.sql`, `0010_loans.sql`, and `0011_portfolio.sql`. Never insert a lower migration after a higher version has shipped.
- The running application and default tests continue to use the legacy migrator throughout Phases 1–7. `src/main.rs`, `src/api/routes.rs`, `src/infrastructure/db.rs`, `src/infrastructure/test_db.rs`, `tests/common/mod.rs`, and `tests/migrations.rs` remain unchanged in this phase.
- The only public V2 construction path is `initialize_v2`, which connects, runs the preflight/migrator, verifies the Finance V2 lineage/latest version, and returns a `VerifiedV2Pool` wrapper. Bare `create_v2_pool`/`migrate_v2` helpers remain infrastructure-private. Initialization must reject any database that already contains legacy SQLx history or application tables but lacks the marker, so a caller cannot construct contexts or start listeners/workers on an unverified pool or accidentally “upgrade” the old database.
- Supabase remains the identity authority. Phase 8 provisions a separate blank Finance V2 database; it will not delete Supabase users, but no local preferences, categories, provider connections, tokens, transactions, subscriptions, or projections are copied.
- Context code must not import another context's domain, application, or infrastructure modules and must not query another context's tables. Cross-context work uses a public facade or an outbox event.
- The shared kernel contains no repository traits, provider types, account kinds, categories, or business workflows.
- All persisted timestamps are `TIMESTAMPTZ`; monetary columns added in later phases use bounded `NUMERIC`, never floating point.

## Context map established by this phase

```text
shared_kernel
  ├── identifiers / money / currency / idempotency / clock
  └── no business-context dependencies

reference_data                 classification                 preferences
  ├── ISO currency catalog       ├── user categories             ├── base currency
  └── public currency query      └── public category query       └── public preferences query

integration
  ├── transactional outbox dispatcher
  ├── idempotent inbox executor
  └── durable process state + fenced leases

Required context skeletons created now:
Ledger, Banking, Mail, Recurring, Reporting, Sharing, Loans, Portfolio

Future upstream direction:
Banking / Loans / Portfolio / Subscriptions ──commands/events──> Ledger
Ledger ──events──> Reporting
```

`Ledger` is upstream-only: it will not know about Monobank, subscriptions, loans, securities, or any provider. Phase 1's module visibility and architecture tests make that rule executable before those contexts exist.

---

## File Map

| File | Action | Responsibility |
|------|--------|----------------|
| `src/shared_kernel/mod.rs` | Create | Export the intentionally small shared kernel |
| `src/shared_kernel/ids.rs` | Create | Universal user/event/correlation IDs and reusable newtype macro; no context aggregate IDs |
| `src/shared_kernel/currency.rs` | Create | Validated uppercase ISO-style `CurrencyCode` value object |
| `src/shared_kernel/money.rs` | Create | Exact `Money` arithmetic with currency mismatch protection |
| `src/shared_kernel/idempotency.rs` | Create | Validated opaque `IdempotencyKey` |
| `src/shared_kernel/clock.rs` | Create | `Clock`, `SystemClock`, and deterministic test clock |
| `src/shared_kernel/events.rs` | Create | Minimal versioned event-envelope metadata shared across contexts |
| `src/contexts/mod.rs` | Create | Export only context public entry points |
| `src/contexts/reference_data/{mod.rs,domain.rs,application.rs,infrastructure.rs,public.rs}` | Create | Currency catalog context |
| `src/contexts/classification/{mod.rs,domain.rs,application.rs,infrastructure.rs,public.rs}` | Create | User-owned categories with archive/version semantics |
| `src/contexts/preferences/{mod.rs,domain.rs,application.rs,infrastructure.rs,public.rs}` | Create | User base-currency preference |
| `src/contexts/{reference_data,classification,preferences}/api/` | Create | Thin DTO/handler/route modules for isolated supporting-context HTTP contracts |
| `src/api/v2.rs` | Create | Isolated V2 router; never mounted by the default runtime in Phase 1 |
| `src/bootstrap/{mod.rs,v2.rs}` | Create | Build supporting contexts/router only from `VerifiedV2Pool` for explicit V2 tests |
| `static/openapi.v2.json` | Create | Parallel unversioned currency/category/preferences contract, extended by later phases |
| `src/integration/mod.rs` | Create | Integration runtime exports |
| `src/integration/outbox.rs` | Create | Integration payload, writer/publisher ports, dispatcher using the shared envelope |
| `src/integration/inbox.rs` | Create | Idempotent consumer execution and receipt contract |
| `src/integration/process_manager.rs` | Create | Durable process state and fenced-lease contract |
| `src/integration/process_managers/mod.rs` | Create | Empty parent module for phase-owned cross-context coordinators added later |
| `src/integration/postgres.rs` | Create | PostgreSQL outbox/inbox/process-manager adapters |
| `src/infrastructure/migrations_v2/.gitkeep` | Create then delete | Let the guarded migrator compile before the first immutable SQL file lands |
| `src/infrastructure/migrations_v2/0001_shared_reference_preferences.sql` | Create | V2 marker, all context schemas, reference/catalog/preferences tables |
| `src/infrastructure/migrations_v2/0002_integration_runtime.sql` | Create | Outbox, inbox receipts, process state, and fenced leases |
| `src/infrastructure/v2_db.rs` | Create | Preflight lineage guard, V2 migrator, and pool creation |
| `src/infrastructure/v2_test_db.rs` | Create | Fresh isolated V2 database test helper |
| `src/lib.rs` | Modify | Export V2 modules without removing legacy modules |
| `src/infrastructure/mod.rs` | Modify | Export V2 database helpers |
| `tests/v2_migrations.rs` | Create | Blank/legacy/marked database migration tests and schema constraints |
| `tests/context_boundaries.rs` | Create | Source-level context import/table ownership guard |
| `tests/shared_kernel.rs` | Create | Value-object contract tests |
| `tests/integration_runtime.rs` | Create | Duplicate delivery, rollback, retry, and lease-fencing tests |
| `tests/v2_foundation.rs` | Create | Isolated end-to-end foundation scenario |
| `tests/supporting_api_v2.rs` | Create | Exact supporting route/auth/version contract tests |
| `tests/openapi_v2.rs` | Create | Parallel OpenAPI parse/route-manifest validation |
| `src/contexts/{ledger,banking,mail,recurring,reporting,sharing,loans,portfolio}/{mod.rs,public.rs}` | Create | Compile-safe context ownership/public-boundary skeletons |

No other source file is changed in Phase 1. In particular, do not touch the legacy migrations, default `src/api/routes.rs`, `main.rs`, or legacy finance handlers.

---

## Task 1: Add the isolated V2 migrator and lineage safety tests

**Files:**
- Create: `src/infrastructure/migrations_v2/.gitkeep`
- Create: `src/infrastructure/v2_db.rs`
- Create: `src/infrastructure/v2_test_db.rs`
- Create: `tests/v2_migrations.rs`
- Modify: `src/infrastructure/mod.rs`

- [x] **Step 1 — RED: write the V2 database contract tests**

Add tests named:

```rust
empty_database_passes_v2_preflight()
marked_v2_database_passes_v2_preflight()
legacy_sqlx_database_is_rejected_before_v2_migrations_run()
nonempty_unmarked_database_is_rejected_before_v2_migrations_run()
```

The test helper must create a uniquely named database in the shared PostgreSQL 16 testcontainer, just as `src/infrastructure/test_db.rs` does. These first tests exercise the preflight query directly, before a V2 root migration exists. The marked case creates only the minimum immutable lineage fixture needed for the preflight contract. The legacy case should run `sqlx::migrate!("src/infrastructure/migrations")` first; it must not invent a fake approximation of the old schema. Task 4 adds end-to-end migration/reopen tests once `0001` exists.

- [x] **Step 2 — Run the tests to verify RED**

Run:

```bash
cargo test --test v2_migrations --no-run
```

Expected: FAIL because `v2_db`, `v2_test_db`, and the V2 migration directory do not exist.

- [x] **Step 3 — GREEN: implement the preflight contract and migrator constant**

Expose:

```rust
pub static V2_MIGRATOR: sqlx::migrate::Migrator =
    sqlx::migrate!("src/infrastructure/migrations_v2");

pub struct VerifiedV2Pool(/* private PgPool */);

pub async fn initialize_v2(database_url: &str) -> anyhow::Result<VerifiedV2Pool>;
pub(crate) async fn create_v2_pool(database_url: &str) -> anyhow::Result<PgPool>;
pub(crate) async fn migrate_v2(pool: &PgPool) -> anyhow::Result<()>;
```

`initialize_v2` calls `migrate_v2`, which must:

1. inspect `to_regclass('_sqlx_migrations')` and known application tables before running SQL;
2. allow a truly empty database;
3. allow a database whose `shared_kernel.database_lineage` singleton is exactly `finance-v2`;
4. reject every other non-empty database with a message that includes `refusing non-Finance-V2 database`;
5. run `V2_MIGRATOR`; and
6. verify the marker again after migration.

Do not implement `DROP`, schema cleanup, or a fallback to the legacy migration root.

- [x] **Step 4 — Run the focused tests**

Run:

```bash
cargo test --test v2_migrations
```

Expected: PASS. The empty migration directory compiles and all four preflight tests pass. No commit in this plan intentionally leaves a failing test; end-to-end initialization remains unasserted until Task 4 creates `0001_shared_reference_preferences.sql`.

- [x] **Step 5 — REFACTOR: make unverified construction inaccessible**

Keep `create_v2_pool` responsible for connection configuration only and put the guard in a separately testable function, but export only `initialize_v2`/`VerifiedV2Pool` to V2 bootstrap and test helpers. The wrapper exposes bounded query/transaction access without a public unchecked constructor. Never log `DATABASE_URL` because it may contain credentials.

- [ ] **Step 6 — Commit boundary**

```bash
git add src/infrastructure/migrations_v2/.gitkeep src/infrastructure/v2_db.rs src/infrastructure/v2_test_db.rs src/infrastructure/mod.rs tests/v2_migrations.rs
git commit -m "feat(infra): add guarded Finance V2 migrator"
```

---

## Task 2: Build typed identifiers and idempotency keys

**Files:**
- Create: `src/shared_kernel/mod.rs`
- Create: `src/shared_kernel/ids.rs`
- Create: `src/shared_kernel/idempotency.rs`
- Create: `tests/shared_kernel.rs`
- Modify: `src/lib.rs`

- [x] **Step 1 — RED: specify identifier and idempotency behavior**

Write tests proving that each identifier:

- wraps `Uuid` without implicit cross-type conversion;
- implements `Copy`, `Eq`, `Hash`, `Display`, `Serialize`, `Deserialize`, and SQLx encode/decode/type support;
- is constructible from an explicit UUID and can generate a V4 UUID; and
- serializes as the canonical UUID string.

Write `IdempotencyKey` tests proving that surrounding whitespace, an empty key, control characters, and more than 200 UTF-8 bytes are rejected. Two keys with the same bytes must compare equal; no normalization other than validation is allowed.

Required universal ID types:

```text
UserId, EventId, CorrelationId, CausationId
```

Also test the reusable macro with a test-only opaque ID. `OutboxMessageId` belongs to `integration`; `CategoryId` belongs to Classification; Ledger and Banking aggregate IDs are introduced by their own contexts in Phases 2 and 3. They may reuse the macro, but the shared kernel must not export those business names.

- [x] **Step 2 — Run the tests to verify RED**

Run: `cargo test --test shared_kernel -- --nocapture`

Expected: FAIL because the shared-kernel modules do not exist.

- [x] **Step 3 — GREEN: implement the smallest typed wrappers**

Use one private macro to remove repetitive trait implementations, but emit real public newtypes rather than aliases. `IdempotencyKey` must redact its value from `Debug`; it can appear in structured logs only as a stable SHA-256 fingerprint added by an infrastructure adapter later.

- [x] **Step 4 — Run focused tests**

Run: `cargo test --test shared_kernel -- --nocapture`

Expected: PASS.

- [x] **Step 5 — REFACTOR: check the shared kernel has no inward dependencies**

Run:

```bash
rg "crate::(contexts|application|domain|infrastructure|api)" src/shared_kernel
```

Expected: no matches.

- [ ] **Step 6 — Commit boundary**

```bash
git add src/shared_kernel src/lib.rs tests/shared_kernel.rs
git commit -m "feat(domain): add Finance V2 typed identifiers"
```

---

## Task 3: Add exact currency and money value objects

**Files:**
- Create: `src/shared_kernel/currency.rs`
- Create: `src/shared_kernel/money.rs`
- Modify: `src/shared_kernel/mod.rs`
- Modify: `tests/shared_kernel.rs`

- [x] **Step 1 — RED: specify currency and money invariants**

Add tests for:

```text
CurrencyCode: accepts UAH/USD/EUR; rejects lowercase, whitespace,
              non-ASCII, and codes that are not exactly three letters.
Money:        preserves Decimal exactly through outbound serialization and
              a checked wire round trip supplied a currency definition;
              adds/subtracts only the same currency;
              rejects currency mismatch instead of converting;
              requires a validated minor-unit scale at construction;
              rejects excess scale and bounded-NUMERIC overflow;
              never exposes f32/f64 constructors;
              supports zero and checked negation.
```

`Money` is an amount and a currency, not an exchange-rate service. Its checked constructor receives the allowed minor-unit scale resolved at the command boundary through `reference_data::public`; `shared_kernel` must not import the Reference Data context or embed a mutable currency catalog. Provider operation quantities/rates use separate value objects and must not relax Ledger Money scale.

- [x] **Step 2 — Run the tests to verify RED**

Run: `cargo test --test shared_kernel -- --nocapture`

Expected: FAIL because `CurrencyCode` and `Money` do not exist.

- [x] **Step 3 — GREEN: implement exact arithmetic**

Back `Money.amount` with `rust_decimal::Decimal`, keep fields private, and require the checked constructor/factory for inbound API/event conversion and application use. Do not derive a public unrestricted `Deserialize` implementation for domain `Money`; raw wire DTOs carry decimal-string amount/currency and are resolved through Reference Data first. Make excess scale, bounds overflow, and mismatched arithmetic return typed errors. Outbound JSON uses decimal strings, never JSON numbers, so clients cannot round money through IEEE-754.

- [x] **Step 4 — Run focused and property-style table tests**

Run: `cargo test --test shared_kernel -- --nocapture`

Expected: PASS, including boundary values near the future database precision.

- [x] **Step 5 — REFACTOR: centralize validation errors**

Keep shared-kernel errors independent of `anyhow`, axum status codes, and SQLx. Application/API adapters will translate them.

- [ ] **Step 6 — Commit boundary**

```bash
git add src/shared_kernel/currency.rs src/shared_kernel/money.rs src/shared_kernel/mod.rs tests/shared_kernel.rs
git commit -m "feat(domain): add exact Money and CurrencyCode values"
```

---

## Task 4: Create the Finance V2 root migration

**Files:**
- Delete: `src/infrastructure/migrations_v2/.gitkeep`
- Create: `src/infrastructure/migrations_v2/0001_shared_reference_preferences.sql`
- Modify: `tests/v2_migrations.rs`

- [x] **Step 1 — RED: add schema and constraint assertions first**

Test that a fresh V2 database has:

- `shared_kernel`, `reference_data`, `classification`, `preferences`, `integration`, `ledger`, `banking`, `mail`, `recurring`, `reporting`, `sharing`, `loans`, and `portfolio` schemas;
- one immutable lineage row equal to `finance-v2`;
- `reference_data.currencies` seeded with at least `UAH`, `USD`, and `EUR`;
- uppercase-three-letter and minor-unit range checks on currencies; and
- tenant/lifecycle/version constraints for categories and preferences; and
- no legacy `public.accounts`, `public.transactions`, or `public.bank_connections` tables.

Also assert that updating/deleting the lineage singleton is rejected.

Add the end-to-end cases deferred from Task 1:

```rust
empty_database_is_initialized_as_finance_v2()
already_marked_v2_database_is_reopened_idempotently()
```

- [x] **Step 2 — Run the migration tests to verify RED**

Run: `cargo test --test v2_migrations empty_database_is_initialized`

Expected: FAIL because migration `0001` is absent.

- [x] **Step 3 — GREEN: create an immediately valid blank-database migration**

The migration must create:

```text
shared_kernel.database_lineage
reference_data.currencies
classification.categories
preferences.user_preferences
```

Use normal indexes. Do not use `CONCURRENTLY`, `NOT VALID`, backfill passes, compatibility triggers, or destructive `DROP` statements; this migration only ever targets an empty V2 database. Protect the singleton lineage row with a trigger that raises on update/delete.

Category `kind` is `income`, `expense`, or `both`; lifecycle is active/archived; active names are unique per user under case-insensitive comparison. Preferences have one row per external Supabase `user_id` and reference an enabled currency. Create the empty owned schemas now so every planned context has an explicit namespace, but do not add placeholder tables or provider/ledger behavior.

- [x] **Step 4 — Run the full migration contract**

Run: `cargo test --test v2_migrations`

Expected: PASS, including the two legacy-database rejection tests.

- [x] **Step 5 — REFACTOR: inspect schema ownership**

Run:

```bash
rg -n "CREATE (TABLE|SCHEMA)|REFERENCES" src/infrastructure/migrations_v2/0001_shared_reference_preferences.sql
```

Expected: every created table is schema-qualified; no legacy `public` table is referenced.

- [ ] **Step 6 — Commit boundary**

```bash
git add src/infrastructure/migrations_v2/.gitkeep src/infrastructure/migrations_v2/0001_shared_reference_preferences.sql tests/v2_migrations.rs
git commit -m "feat(db): create Finance V2 root migration"
```

---

## Task 5: Define clock, outbox, inbox, and process-manager contracts

**Files:**
- Create: `src/shared_kernel/clock.rs`
- Create: `src/shared_kernel/events.rs`
- Create: `src/integration/mod.rs`
- Create: `src/integration/outbox.rs`
- Create: `src/integration/inbox.rs`
- Create: `src/integration/process_manager.rs`
- Create: `src/integration/process_managers/mod.rs`
- Modify: `src/shared_kernel/mod.rs`
- Modify: `src/lib.rs`
- Modify: `tests/shared_kernel.rs`
- Create: `tests/integration_runtime.rs`

- [x] **Step 1 — RED: test deterministic time and integration contracts**

Write tests proving that a fixed clock returns the same UTC instant across a command and that shared-kernel `EventEnvelope` metadata carries a typed event ID, context, aggregate identity/version, event type/schema version, tenant, occurrence time, and correlation/causation IDs. `integration::IntegrationEvent` combines that envelope with a JSON payload. Add compile-time test doubles for:

```rust
pub trait OutboxWriter {
    async fn append(&mut self, event: &IntegrationEvent) -> Result<(), OutboxError>;
}

pub trait InboxExecutor {
    async fn execute_once<T>(
        &mut self,
        consumer: &ConsumerName,
        message: &IntegrationEvent,
        action: T,
    ) -> Result<InboxOutcome, InboxError>;
}

pub trait ProcessManagerStore {
    async fn acquire_lease(&mut self, key: &ProcessKey, holder: &str, ttl: Duration)
        -> Result<FencedLease, ProcessError>;
    async fn save(&mut self, state: &ProcessState, lease: &FencedLease)
        -> Result<(), ProcessError>;
}
```

Mutable receivers are intentional: context UoWs pass transaction-bound adapters. Inbox duplicate detection and its local side effects must commit in the same database transaction. A process update must carry the current monotonically increasing fencing token.

- [x] **Step 2 — Run the tests to verify RED**

Run:

```bash
cargo test --test shared_kernel clock
cargo test --test integration_runtime contracts
```

Expected: FAIL because the clock and integration contracts do not exist.

- [x] **Step 3 — GREEN: implement the contracts**

Provide `SystemClock` and a fake clock whose instant is injected. Put only the versioned envelope metadata in `shared_kernel::events`; define provider-neutral integration payload, receipt, process-key/state, retry, and fenced-lease types under `integration`. Declare `pub mod process_managers;` in `integration/mod.rs`; its Phase 1 child module is intentionally empty so later phases can add coordinators without changing the parent-module contract. Keep ports free of SQLx and tokio task spawning; Task 6 supplies the PostgreSQL adapters and dispatcher runtime.

- [x] **Step 4 — Run focused tests**

Run:

```bash
cargo test --test shared_kernel clock
cargo test --test integration_runtime contracts
```

Expected: PASS.

- [x] **Step 5 — REFACTOR: enforce a data-minimal event envelope**

Remove any convenience field that could invite credentials, access tokens, full email bodies, or raw provider payloads into events/process state. Events carry IDs and minimum business facts. Define explicit payload schema versions so consumers can reject unsupported versions rather than guessing.

- [ ] **Step 6 — Commit boundary**

```bash
git add src/shared_kernel/clock.rs src/shared_kernel/events.rs src/shared_kernel/mod.rs src/integration src/lib.rs tests/shared_kernel.rs tests/integration_runtime.rs
git commit -m "feat(integration): define durable messaging and process contracts"
```

---

## Task 6: Implement the concrete integration runtime

**Files:**
- Create: `src/infrastructure/migrations_v2/0002_integration_runtime.sql`
- Create: `src/integration/postgres.rs`
- Modify: `src/integration/{mod.rs,outbox.rs,inbox.rs,process_manager.rs}`
- Modify: `tests/v2_migrations.rs`
- Modify: `tests/integration_runtime.rs`

- [x] **Step 1 — RED: write rollback, duplicate, retry, and fencing tests**

Test that:

- rolling back a context transaction also rolls back its outbox append;
- two dispatchers using `FOR UPDATE SKIP LOCKED` never own one claim concurrently;
- a crash after publish but before acknowledgment causes at-least-once redelivery;
- the inbox executes local side effects exactly once under duplicate and concurrent delivery;
- same message ID is independent across consumer names but unique within one consumer;
- failed delivery records bounded/redacted error text, exponential retry time, and dead-letter status after a configured attempt cap;
- process state compare-and-swap rejects stale versions;
- lease acquisition increments a durable fencing token; and
- a former holder cannot save state after expiry/reacquisition even if its process resumes.

- [x] **Step 2 — Run the tests to verify RED**

Run: `cargo test --test integration_runtime`

Expected: FAIL because migration `0002` is absent.

- [x] **Step 3 — GREEN: add immediately valid tables and PostgreSQL adapters**

Create:

```text
integration.outbox_messages
integration.inbox_receipts
integration.process_instances
integration.process_leases
```

The outbox includes ordered sequence, message/schema version, context/aggregate/user, correlation/causation, payload, availability, claim holder/token/expiry, attempts, publication/dead-letter timestamps, and bounded last error. Inbox uniqueness is `(consumer_name, message_id)`. Process instances use `(process_name, instance_key)` plus JSONB state/status/version/next wake-up. Leases use the same key, holder, expiry, and monotonically increasing fencing token.

Implement a bounded-batch `OutboxDispatcher<P: EventPublisher>` and transaction-bound inbox/process adapters. The runtime may be invoked by isolated tests now; do **not** spawn it from `main.rs` before Phase 8.

- [x] **Step 4 — Run migration tests**

Run:

```bash
cargo test --test v2_migrations
cargo test --test integration_runtime
```

Expected: PASS.

- [x] **Step 5 — REFACTOR: make delivery semantics explicit**

Document/test that outbox delivery is at-least-once, inbox side effects are exactly-once within this PostgreSQL boundary, and external publishers may receive duplicates. Ensure no adapter holds a database transaction open during network publication.

- [ ] **Step 6 — Commit boundary**

```bash
git add src/infrastructure/migrations_v2/0002_integration_runtime.sql src/integration tests/v2_migrations.rs tests/integration_runtime.rs
git commit -m "feat(integration): add durable V2 messaging and process runtime"
```

---

## Task 7: Implement the Reference Data public facade

**Files:**
- Create: `src/contexts/mod.rs`
- Create: `src/contexts/reference_data/mod.rs`
- Create: `src/contexts/reference_data/domain.rs`
- Create: `src/contexts/reference_data/application.rs`
- Create: `src/contexts/reference_data/infrastructure.rs`
- Create: `src/contexts/reference_data/public.rs`
- Modify: `src/lib.rs`
- Create: `tests/reference_data.rs`

- [x] **Step 1 — RED: write public-contract and repository tests**

Test the public facade, not its private implementation:

```rust
pub trait CurrencyCatalog {
    async fn require_enabled(&self, code: CurrencyCode) -> Result<CurrencyDefinition, CurrencyError>;
    async fn list_enabled(&self) -> Result<Vec<CurrencyDefinition>, CurrencyError>;
}
```

Cover enabled currency lookup, disabled currency rejection, deterministic ordering, and database error translation that does not leak SQL text.

- [x] **Step 2 — Run the tests to verify RED**

Run: `cargo test --test reference_data`

Expected: FAIL because the context does not exist.

- [x] **Step 3 — GREEN: implement private layers and a public facade**

`mod.rs` keeps `domain`, `application`, and `infrastructure` private and re-exports only `public`. The PostgreSQL adapter queries only `reference_data.*`. Avoid a generic repository base trait.

- [x] **Step 4 — Run focused tests**

Run: `cargo test --test reference_data`

Expected: PASS.

- [x] **Step 5 — REFACTOR: make ownership visible in names**

Use `PgCurrencyCatalog` inside the context and export the capability as `CurrencyCatalog`. Do not reuse the legacy `FxRateRepository`; exchange rates are not currency definitions.

- [ ] **Step 6 — Commit boundary**

```bash
git add src/contexts/mod.rs src/contexts/reference_data src/lib.rs tests/reference_data.rs
git commit -m "feat(reference-data): expose the V2 currency catalog"
```

---

## Task 8: Implement Classification and Preferences public facades

**Files:**
- Create: `src/contexts/classification/{mod.rs,domain.rs,application.rs,infrastructure.rs,public.rs}`
- Create: `src/contexts/preferences/{mod.rs,domain.rs,application.rs,infrastructure.rs,public.rs}`
- Modify: `src/contexts/mod.rs`
- Create: `tests/classification_preferences.rs`

- [x] **Step 1 — RED: write aggregate and adapter tests**

Classification tests must cover create, rename with `expected_version`, archive, restore, repeated archive/restore semantics, cross-user invisibility, and duplicate active names. Preferences tests must cover default `UAH`, explicit enabled currency, compare-and-swap version updates, and cross-user isolation.

- [x] **Step 2 — Run the tests to verify RED**

Run: `cargo test --test classification_preferences`

Expected: FAIL because both contexts are absent.

- [x] **Step 3 — GREEN: implement one aggregate repository per context**

Expose command/query DTOs through each `public.rs`. Infrastructure modules own SQL and translate rows. Updates use `WHERE user_id = $1 AND id = $2 AND version = $3`, increment version, and distinguish not-found from version-conflict without revealing another tenant's row.

No Classification code may query Ledger tables or import a future Ledger type. Classification owns `CategoryId` and exports it through `classification::public`; a future journal annotation accepts that opaque public ID after validating it through the same facade.

- [x] **Step 4 — Run focused tests**

Run: `cargo test --test classification_preferences`

Expected: PASS.

- [x] **Step 5 — REFACTOR: separate commands from read models**

Do not return persistence rows from the public facades. Keep mutation DTOs and query DTOs explicit so reporting can later consume events rather than repositories.

- [ ] **Step 6 — Commit boundary**

```bash
git add src/contexts/classification src/contexts/preferences src/contexts/mod.rs tests/classification_preferences.rs
git commit -m "feat: add V2 classification and preferences contexts"
```

---

## Task 9: Create every context skeleton and make boundaries executable

**Files:**
- Create: `src/contexts/{ledger,banking,mail,recurring,reporting,sharing,loans,portfolio}/mod.rs`
- Create: `src/contexts/{ledger,banking,mail,recurring,reporting,sharing,loans,portfolio}/public.rs`
- Create: `tests/context_boundaries.rs`
- Modify: `src/contexts/mod.rs`
- Modify: `src/contexts/{reference_data,classification,preferences}/mod.rs`

- [x] **Step 1 — RED: write an architecture test with a deliberate fixture violation**

First assert that all required context roots and public entry points exist: Ledger, Banking, Mail, Recurring, Reporting, Sharing, Loans, Portfolio, Reference Data, Classification, and Preferences. Then recursively inspect both `src/contexts` and `src/integration/process_managers`. For a context named `X`, fail if its source imports `crate::contexts::Y::{domain,application,infrastructure,api}` for `Y != X`, or if its SQL string contains another owned schema. A process manager may import multiple contexts' `public` contracts, but it may not import any context's domain/application/infrastructure/API layer, contain SQL against a context-owned schema, or downcast to a concrete repository. Allow:

```text
crate::shared_kernel
crate::integration
crate::contexts::<other>::public
```

Add temporary test fixtures proving the checker detects a forbidden Rust import and foreign-schema SQL in both a context and a process-manager source. Remove the fixture violations before GREEN.

- [x] **Step 2 — Run the test to verify RED**

Run: `cargo test --test context_boundaries`

Expected: FAIL on the deliberate fixture.

- [x] **Step 3 — GREEN: tighten module visibility and remove the fixture violation**

Concrete context roots should follow:

```rust
mod application;
mod domain;
mod infrastructure;
pub mod public;
```

A Phase 1 skeleton contains only `pub mod public;` and an ownership marker/empty public contract; its later phase adds private layers. Do not invent placeholder domain aggregates merely to fill a directory. The test itself remains and passes against the repository.

- [x] **Step 4 — Run focused and compile tests**

Run:

```bash
cargo test --test context_boundaries
cargo check --all-targets
```

Expected: PASS.

- [x] **Step 5 — REFACTOR: document narrow exceptions in the test**

The only database exception is a one-way composite foreign key to immutable shared/reference identifiers. There are no cross-context triggers, repositories, or writes.

- [ ] **Step 6 — Commit boundary**

```bash
git add tests/context_boundaries.rs src/contexts
git commit -m "feat(architecture): establish every Finance V2 context boundary"
```

---

## Task 10: Expose supporting contexts through the isolated V2 API

**Files:**
- Create: `src/contexts/reference_data/api/{mod.rs,dto.rs,handlers.rs,routes.rs}`
- Create: `src/contexts/classification/api/{mod.rs,dto.rs,handlers.rs,routes.rs}`
- Create: `src/contexts/preferences/api/{mod.rs,dto.rs,handlers.rs,routes.rs}`
- Modify: `src/contexts/{reference_data,classification,preferences}/mod.rs`
- Create: `src/api/v2.rs`
- Create: `src/bootstrap/{mod.rs,v2.rs}`
- Modify: `src/api/mod.rs`
- Create: `static/openapi.v2.json`
- Create: `tests/supporting_api_v2.rs`
- Create: `tests/openapi_v2.rs`

- [x] **Step 1 — RED: freeze exact route and concurrency contracts**

Construct only the isolated V2 router and require these exact future-unversioned paths:

```text
GET   /currencies
GET   /currencies/{code}
POST  /categories
GET   /categories
GET   /categories/{id}
PATCH /categories/{id}
POST  /categories/{id}/archive
POST  /categories/{id}/restore
GET   /preferences
PATCH /preferences
```

All routes use the repository's existing authenticated-user boundary, including the read-only currency catalog. Category rename/archive/restore and preference update require body `expected_version` and reject missing/stale values. Category creation starts at version 1. An absent preference read returns the effective default `UAH`, `version: 0`, and `persisted: false` without inserting a row; `PATCH /preferences` with `expected_version: 0` creates version 1, while later updates use normal compare-and-swap. Preference update validates the selected enabled currency through `reference_data::public`. Reads are side-effect free, tenant scoped where applicable, include `version`/`as_of`, and expose archived categories without hiding history. No hard-delete route exists. These are metadata commands, not financial commands, so `Idempotency-Key` is not required; optimistic version/unique-name rules make their replay outcome explicit.

- [x] **Step 2 — Run RED**

```bash
cargo test --test supporting_api_v2 -- --nocapture
cargo test --test openapi_v2 -- --nocapture
```

Expected: FAIL because the isolated supporting router/OpenAPI do not exist.

- [x] **Step 3 — GREEN: implement thin context-owned adapters**

Handlers derive `UserId` from auth, map DTOs, and call only their owning context public facade. They contain no SQL or cross-context repository access. `bootstrap::v2` accepts only `VerifiedV2Pool`, builds the three supporting contexts, and returns the isolated router without spawning workers or changing `main.rs`/default routes. `static/openapi.v2.json` is the single parallel V2 manifest that later phases extend.

- [x] **Step 4 — Run focused API/OpenAPI tests**

```bash
cargo test --test supporting_api_v2 -- --nocapture
cargo test --test openapi_v2 -- --nocapture
```

Expected: PASS while legacy API tests remain green.

- [x] **Step 5 — REFACTOR: prove isolation and exactness**

Assert every OpenAPI operation has one matching isolated route, no `/v2` prefix exists, context handlers contain no SQL, and `src/main.rs`/`src/api/routes.rs` have no diff.

- [ ] **Step 6 — Commit boundary**

```bash
git add src/contexts/reference_data/api src/contexts/classification/api src/contexts/preferences/api src/contexts/reference_data/mod.rs src/contexts/classification/mod.rs src/contexts/preferences/mod.rs src/api/v2.rs src/api/mod.rs src/bootstrap static/openapi.v2.json tests/supporting_api_v2.rs tests/openapi_v2.rs
git commit -m "feat(api): add isolated v2 supporting context routes"
```

---

## Task 11: Prove the isolated foundation end to end

**Files:**
- Create: `tests/v2_foundation.rs`
- Modify: `src/integration/postgres.rs`

- [x] **Step 1 — RED: write one cross-foundation scenario**

Using only `v2_test_db`, public supporting-context facades, and integration ports:

1. migrate a unique blank database through `0002`;
2. resolve `UAH`, create a category, and set preferences;
3. begin one SQLx transaction, write a representative future-context state change plus outbox event, then roll it back and assert both disappear;
4. repeat and commit;
5. dispatch through a publisher that simulates crash-after-publish;
6. redeliver and consume twice through one inbox consumer, asserting its local side effect occurs once; and
7. let holder A's process lease expire, acquire holder B's higher fencing token, and prove A cannot overwrite B.

- [x] **Step 2 — Run the scenario to verify RED**

Run: `cargo test --test v2_foundation`

Expected: FAIL until all concrete adapters compose through one transaction.

- [x] **Step 3 — GREEN: add only missing adapter composition**

Do not introduce a service locator or a pool-backed shortcut. If a future context UoW needs to append outbox or execute inbox work, it must be able to construct the adapter from `&mut sqlx::Transaction<Postgres>`.

- [x] **Step 4 — Run the focused scenario**

Run:

```bash
cargo test --test v2_foundation
```

Expected: PASS.

- [x] **Step 5 — REFACTOR: prove this is still a parallel build**

Verify `src/main.rs`, `src/api/routes.rs`, `src/infrastructure/db.rs`, `src/infrastructure/test_db.rs`, `tests/common/mod.rs`, `tests/migrations.rs`, `docker-compose.yml`, and `.env.example` have no Phase 1 diff. The V2 helper is reachable only from explicit V2 tests until Phase 8.

- [ ] **Step 6 — Commit boundary**

```bash
git add tests/v2_foundation.rs src/integration/postgres.rs
git commit -m "test: verify the isolated Finance V2 foundation"
```

---

## Task 12: Phase 1 final verification and handoff

**Files:**
- Read: all Phase 1 files
- Do not modify legacy finance source or migrations

- [x] **Step 1 — Verify formatting and compilation**

Run:

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --test shared_kernel
cargo test --test v2_migrations
cargo test --test integration_runtime
cargo test --test v2_foundation
cargo test --test reference_data
cargo test --test classification_preferences
cargo test --test context_boundaries
cargo test --test supporting_api_v2
cargo test --test openapi_v2
```

Expected: PASS.

- [x] **Step 2 — Verify legacy checksum history remains frozen**

Run:

```bash
git diff --exit-code -- src/infrastructure/migrations tests/migrations.rs
cargo test --test migrations
```

Expected: no legacy migration diff and the existing checksum suite PASSes.

- [x] **Step 3 — Verify Phase 1 did not cut over production**

Run:

```bash
rg -n 'migrate!\("src/infrastructure/migrations"\)' src/infrastructure/db.rs src/infrastructure/test_db.rs tests/common/mod.rs tests/migrations.rs
rg -n 'create_pool' src/main.rs
```

Expected: legacy runtime/test paths are still present. `initialize_v2`/`VerifiedV2Pool` and the integration runtime are compiled/tested but not called by `main.rs`.

- [x] **Step 4 — Record the irreversible Phase 8 precondition**

The Phase 8 cutover checklist must state that the target `DATABASE_URL` is a newly provisioned/blank Finance V2 database, the old Docker named volume is not mounted, provider connections must be re-established, and no legacy finance data is expected. Phases 2–7 continue to use `v2_test_db` explicitly.

- [ ] **Step 5 — Final commit only if verification required changes**

```bash
git add <only-files-changed-by-verification>
git commit -m "test: complete Finance V2 foundation verification"
```

---

## Verification commands

Task 12 is the canonical Phase 1 gate. At minimum, run:

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --test shared_kernel
cargo test --test v2_migrations
cargo test --test integration_runtime
cargo test --test v2_foundation
cargo test --test reference_data
cargo test --test classification_preferences
cargo test --test context_boundaries
cargo test --test supporting_api_v2
cargo test --test openapi_v2
cargo test --test migrations
git diff --exit-code -- src/infrastructure/migrations tests/migrations.rs
```

All commands must pass while the legacy default bootstrap remains unchanged.

## Commit boundaries

| Commit | Required outcome |
|--------|------------------|
| `feat(infra): add guarded Finance V2 migrator` | V2/legacy database discrimination exists before migrations run |
| `feat(domain): add Finance V2 typed identifiers` | IDs and idempotency key cannot be confused or accepted unchecked |
| `feat(domain): add exact Money and CurrencyCode values` | Money arithmetic is decimal and currency-safe |
| `feat(db): create Finance V2 root migration` | Blank V2 schemas, marker, currency, classification, and preferences exist |
| `feat(integration): define durable messaging and process contracts` | Deterministic time and transaction-bound runtime contracts exist |
| `feat(integration): add durable V2 messaging and process runtime` | Outbox/inbox/process tables, adapters, dispatch, retry, and fencing work |
| `feat(reference-data): expose the V2 currency catalog` | Currency semantics are available through a public facade |
| `feat: add V2 classification and preferences contexts` | Supporting contexts are usable without cross-table access |
| `feat(architecture): establish every Finance V2 context boundary` | All roadmap contexts exist and forbidden imports/queries fail CI |
| `feat(api): add isolated v2 supporting context routes` | Currency/category/preferences operations are exact and testable without runtime cutover |
| `test: verify the isolated Finance V2 foundation` | The parallel foundation composes without touching runtime |

Do not squash migration safety, shared-kernel invariants, and integration delivery semantics into one opaque commit. Later phases must be able to bisect the parallel foundation independently of the Phase 8 cutover.

## Exit criteria

- [x] A fresh PostgreSQL 16 database migrates from zero to V2 `0002` and is marked `finance-v2`.
- [x] A legacy or arbitrary non-empty database is rejected before V2 SQL executes.
- [x] Legacy migration files and frozen checksum tests are byte-for-byte unchanged.
- [x] `Money` cannot mix currencies or round through floating point; API serialization uses decimal strings.
- [x] Typed IDs and validated idempotency keys are available without business-context dependencies.
- [x] Reference Data, Classification, and Preferences expose public facades and own their tables.
- [x] Currency, category lifecycle, and base-currency preference APIs are exact in the isolated router/OpenAPI; versioned metadata changes are reachable without mounting V2 by default.
- [x] Outbox append rolls back with a caller UoW; dispatch is at-least-once and does not hold a transaction during network publication.
- [x] Inbox duplicate/concurrent delivery executes local side effects once in the same PostgreSQL transaction.
- [x] Durable process state uses compare-and-swap and fenced leases; an expired holder cannot overwrite a successor.
- [x] Ledger, Banking, Mail, Recurring, Reporting, Sharing, Loans, Portfolio, and supporting context skeletons compile.
- [x] Architecture tests reject cross-context private imports and cross-context SQL.
- [x] The legacy application still starts unchanged; no partial V2 runtime cutover has occurred.

## Explicitly out of scope

- Ledger accounts, journal entries, postings, balances, transfers, corrections, and reversals (Phase 2).
- Monobank tokens, resources, webhooks, inbox events, synchronization, and provider balance observations (Phase 3).
- Behavior inside Loans, Portfolio/ОВДП, Recurring, Reporting, Sharing, Mail, and Banking; Phase 1 creates boundaries only.
- Copying or reconciling any current financial data.
