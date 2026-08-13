# Financial Core V2 — Phase 7: Portfolio and Manual ОВДП

> **For agentic workers:** REQUIRED SUB-SKILL: Use `superpowers:subagent-driven-development` (recommended) or `superpowers:executing-plans` when available. Execute tasks in order and keep checkbox state in this file.

**Goal:** Add a Portfolio bounded context that tracks manual ОВДП instruments, immutable position activity, lots, realized results, append-only valuations, and optional one-time Ledger cash settlement without treating securities as cash-account balances.

**Dependencies:** Phases 1–6 are integrated and migrations pass through `0010`. The shared `Money`, currency, universal IDs, event envelope, idempotency, outbox/inbox, and process-manager runtime are stable. Ledger publishes the typed Portfolio cash-settlement command and journal completion events. Reporting exposes a versioned Portfolio event consumer contract.

**Architecture:** Portfolio owns instruments, accounts, position transactions, lots, and valuation. Purchases and opening positions create lots; sales and redemptions dispose explicit lots or FIFO lots under a per-position lock. Posted Portfolio transactions are immutable; correction and reversal append new facts. Optional cash effects are orchestrated through a durable process manager and a typed Ledger command with a derived idempotency key. Ledger cash/control balances and Portfolio market value remain separate and are combined only by Reporting.

**Tech Stack:** Rust 2024, SQLx/PostgreSQL 16, Axum, `rust_decimal`, Serde, UUID, Chrono, proptest-style deterministic/property tests using the repository's chosen test dependency, Testcontainers.

**Spec:** `docs/superpowers/specs/2026-08-05-finance-ddd-v2-design.md`

---

## Test-first task protocol

For Tasks 1–10, the first test/check step is **RED**, the smallest implementation step is **GREEN**, and the boundary/security/duplication review before commit is **REFACTOR** even where the step title is shortened. Record the expected RED failure, run the named GREEN tests, and do not commit with a known failing prior-phase test. Task 11 is the phase verification/handoff gate and introduces no untested behavior.

## Scope boundaries

V1 supports:

- User-owned manual ОВДП instruments.
- Manual portfolio accounts.
- Opening positions, buys, sells, coupons, maturity/redemption, position corrections, and reversals.
- Explicit acquisition cost, proceeds, fees, and source/provenance.
- Explicit lot disposal or FIFO fallback.
- Manual price plus accrued-interest valuations.
- Whole-unit ОВДП quantities and absolute instrument-currency price-per-bond quotes; percent-of-face/yield input is not silently inferred.
- Optional same-currency Ledger cash settlement.

V1 does not support broker synchronization, live prices, tax reports, amortized-cost/yield calculations, automatic coupon schedules, or inventing values for missing historical data. Missing acquisition cost is an explicit state and blocks realized-gain output for the affected quantity.

## File map

| File | Action |
|---|---|
| `src/infrastructure/migrations_v2/0011_portfolio.sql` | Create — Portfolio schema, scoped command receipts, immutable facts, lots, projections, constraints |
| `src/contexts/portfolio/mod.rs` | Modify Phase 1 skeleton — context module exports |
| `src/contexts/portfolio/public.rs` | Modify Phase 1 skeleton — public command/query façade and v1 integration events |
| `src/contexts/portfolio/domain/instrument.rs` | Create — `Instrument` aggregate and ОВДП terms |
| `src/contexts/portfolio/domain/account.rs` | Create — `PortfolioAccount` aggregate/lifecycle |
| `src/contexts/portfolio/domain/transaction.rs` | Create — immutable Portfolio transaction aggregate |
| `src/contexts/portfolio/domain/lot.rs` | Create — lots, allocations, FIFO engine |
| `src/contexts/portfolio/domain/valuation.rs` | Create — append-only valuation snapshot |
| `src/contexts/portfolio/domain/error.rs` | Create — stable domain errors |
| `src/contexts/portfolio/domain/mod.rs` | Create — domain exports |
| `src/contexts/portfolio/application/commands.rs` | Create — task command DTOs |
| `src/contexts/portfolio/application/handlers.rs` | Create — command handlers/UoW orchestration |
| `src/contexts/portfolio/application/queries.rs` | Create — instrument/account/activity/position read DTOs |
| `src/contexts/portfolio/application/ports.rs` | Create — repositories, UoW, projections, Ledger port |
| `src/contexts/portfolio/application/mod.rs` | Create — application exports |
| `src/contexts/portfolio/infrastructure/repository.rs` | Create — aggregate persistence |
| `src/contexts/portfolio/infrastructure/unit_of_work.rs` | Create — SQLx UoW |
| `src/contexts/portfolio/infrastructure/projection.rs` | Create — position and valuation projections/rebuild |
| `src/contexts/portfolio/infrastructure/queries.rs` | Create — read-side SQL |
| `src/contexts/portfolio/infrastructure/mod.rs` | Create — infrastructure exports |
| `src/contexts/portfolio/api/dto.rs` | Create — decimal-string HTTP DTOs |
| `src/contexts/portfolio/api/handlers.rs` | Create — task-oriented handlers |
| `src/contexts/portfolio/api/routes.rs` | Create — isolated Portfolio router |
| `src/contexts/portfolio/api/mod.rs` | Create — API exports |
| `src/integration/process_managers/portfolio_cash_settlement.rs` | Create — durable Ledger settlement coordinator |
| `src/integration/process_managers/mod.rs` | Modify — register Portfolio process manager |
| `src/contexts/reporting/public.rs` | Modify — accept Portfolio v1 events |
| `src/contexts/reporting/application/projectors.rs` | Modify — register Portfolio event handlers in the Reporting dispatcher |
| `src/contexts/reporting/infrastructure/portfolio_projection.rs` | Create — value/net-worth projection consumer |
| `src/contexts/reporting/infrastructure/mod.rs` | Modify — export the Portfolio projection adapter |
| `src/api/v2.rs` | Modify — compose Portfolio routes into the isolated replacement router |
| `src/bootstrap/v2.rs` | Modify — construct Portfolio components without mounting legacy/default routes |
| `src/contexts/mod.rs` | Modify — export Portfolio context |
| `static/openapi.v2.json` | Modify — add Portfolio task/read schemas and routes |
| `tests/portfolio_domain.rs` | Create — aggregates, arithmetic, reversal tests |
| `tests/portfolio_lots.rs` | Create — FIFO/explicit lot property tests |
| `tests/portfolio_persistence.rs` | Create — constraints, immutability, projection tests |
| `tests/portfolio_api.rs` | Create — auth/idempotency/version/API contract tests |
| `tests/portfolio_cash_settlement.rs` | Create — process-manager crash/retry/correlation tests |
| `tests/reporting_portfolio.rs` | Create — valuation/net-worth/no-double-count tests |

Paths assume the context skeleton and `static/openapi.v2.json` were established by earlier phases. If an earlier phase names a shared test helper differently, use that existing helper rather than creating a duplicate.

---

## Task 1: Create the Portfolio schema and database invariants

**Files:**

- Create: `src/infrastructure/migrations_v2/0011_portfolio.sql`
- Create: `tests/portfolio_persistence.rs`

- [ ] **Step 1: Write failing fresh-database tests**

Cover schema ownership and tenant-safe constraints for:

- `portfolio.instruments` with `UNIQUE (id, user_id)` and a user-scoped unique manual identity such as `(user_id, identifier_kind, identifier)`.
- `portfolio.accounts` with `UNIQUE (id, user_id)` and archive/version columns.
- `portfolio.command_receipts` unique on `(user_id, command_scope, idempotency_key)` with canonical request hash, processing/terminal status, stable durable result/HTTP status, and timestamps.
- `portfolio.transactions` with aggregate version/status, effective/recorded timestamps, source, optional `reversal_of`, and correlation.
- `portfolio.transaction_components` for typed quantities and Money components; components are not a generic public posting API.
- `portfolio.position_lots` and `portfolio.lot_allocations` with composite tenant FKs.
- `portfolio.position_projection` and `portfolio.valuation_snapshots`.
- Portfolio outbox linkage/process correlation fields.

Tests must reject cross-user account/instrument/transaction/lot relationships, negative or zero quantities where prohibited, currency mismatches, disposal beyond available quantity, duplicate reversal, and update/delete of posted transactions/components/allocations. Concurrent claims for the same scoped key/hash converge on one receipt/result; the same scoped key with a different canonical hash is a conflict, while a distinct documented scope is independent.

- [ ] **Step 2: Run the tests and capture the expected failure**

```bash
SQLX_OFFLINE=true cargo test --test portfolio_persistence schema_ -- --nocapture
```

Expected: FAIL because `portfolio` tables do not exist.

- [ ] **Step 3: Add the migration**

Use bounded `NUMERIC` columns compatible with the shared Decimal policy and `TIMESTAMPTZ`. Create normal transactional indexes because V2 is a blank baseline. Include stable ordering indexes such as `(user_id, effective_at DESC, sequence DESC, id DESC)` and `(account_id, instrument_id, acquired_at, id)` for FIFO.

Do not add a scalar mutable `balance` column to a Portfolio account. Position projection rows are derived caches and must be distinguishable from immutable transaction/lot facts.

- [ ] **Step 4: Make database tests pass**

```bash
SQLX_OFFLINE=true cargo test --test portfolio_persistence schema_ -- --nocapture
```

- [ ] **Step 5: Commit**

```bash
git add src/infrastructure/migrations_v2/0011_portfolio.sql tests/portfolio_persistence.rs
git commit -m "feat(portfolio): add v2 portfolio schema"
```

---

## Task 2: Implement Instrument and PortfolioAccount aggregates

**Files:**

- Create: `src/contexts/portfolio/domain/instrument.rs`
- Create: `src/contexts/portfolio/domain/account.rs`
- Create: `src/contexts/portfolio/domain/error.rs`
- Create: `src/contexts/portfolio/domain/mod.rs`
- Create: `tests/portfolio_domain.rs`

- [ ] **Step 1: Write failing aggregate tests**

Test that a manual ОВДП instrument requires:

- a non-empty ISIN or stable manual identifier;
- ISO currency and positive face value;
- issuer/type (`SovereignBond`/`Ovdp`);
- issue date not after maturity date;
- explicit coupon terms (`Fixed`, `ZeroCoupon`, or `Unknown`) without inventing a schedule;
- source (`Manual`) and optimistic version.

Test account create/rename/archive/restore transitions and stale `expected_version` rejection. An archived account rejects new ordinary Portfolio activity but still permits reversal.

- [ ] **Step 2: Run and verify failure**

```bash
cargo test --test portfolio_domain -- --nocapture
```

- [ ] **Step 3: Implement the aggregates and value objects**

Keep ISIN validation syntactic in V1; do not claim that an identifier exists in an external registry. Preserve unknown coupon/acquisition information explicitly.

- [ ] **Step 4: Run focused tests**

```bash
cargo test --test portfolio_domain -- --nocapture
```

- [ ] **Step 5: Commit**

```bash
git add src/contexts/portfolio/domain tests/portfolio_domain.rs
git commit -m "feat(portfolio): model instruments and portfolio accounts"
```

---

## Task 3: Model immutable Portfolio transactions

**Files:**

- Create: `src/contexts/portfolio/domain/transaction.rs`
- Modify: `src/contexts/portfolio/domain/mod.rs`
- Modify: `tests/portfolio_domain.rs`

- [ ] **Step 1: Write failing tests for every transaction type**

Cover:

- `OpeningPosition`: positive quantity, acquisition date, known acquisition cost or explicit `UnknownCost`, required reason/source.
- `Buy`: positive quantity, trade date, settlement currency, explicit total acquisition cost, optional fees/accrued-interest components.
- `Sell`: positive quantity, proceeds, optional fees, and optional explicit lot allocations.
- `Coupon`: zero quantity effect, positive coupon Money, ex/payment date metadata.
- `Redemption`: disposal quantity, proceeds, and maturity/reference metadata.
- `PositionCorrection`: signed quantity/cost delta with mandatory reason; it cannot disguise a normal buy/sell.

For an `Ovdp` instrument, quantity must have scale zero (whole bonds) in V1. Acquisition cost, accrued interest, and fees remain separately persisted components; the plan's realized result is a documented book result, not a Ukrainian tax calculation. Future fractional instruments require an instrument-specific quantity policy rather than weakening the ОВДП invariant.

All Money components for one instrument transaction must use the instrument currency in V1. Posted transactions expose recorded/effective time, actor, source, correlation, and immutable status.

- [ ] **Step 2: Add reversal-symmetry tests**

An exact reversal points to one original transaction, negates its position/cost effects, cannot itself be reversed twice, and never overwrites the original. Coupon and cash-settlement reversals retain their correlation chain.

- [ ] **Step 3: Run and verify failure**

```bash
cargo test --test portfolio_domain -- --nocapture
```

- [ ] **Step 4: Implement and make tests pass**

Use named constructors/commands rather than a caller-provided enum plus arbitrary component list. Keep calculated fields out of constructors; the application service supplies locked lot allocations and projection results.

- [ ] **Step 5: Commit**

```bash
git add src/contexts/portfolio/domain/transaction.rs src/contexts/portfolio/domain/mod.rs tests/portfolio_domain.rs
git commit -m "feat(portfolio): model immutable position activity"
```

---

## Task 4: Implement deterministic lot accounting

**Files:**

- Create: `src/contexts/portfolio/domain/lot.rs`
- Modify: `src/contexts/portfolio/domain/mod.rs`
- Create: `tests/portfolio_lots.rs`

- [ ] **Step 1: Write property and example tests**

Prove:

- opening/buy creates lots with quantity and cost basis;
- explicit allocations must name existing same-user/account/instrument lots and sum to the disposal quantity;
- FIFO consumes `(acquired_at, created_sequence, lot_id)` in total order;
- partial disposal retains exact remaining quantity and proportional stored cost according to the documented Decimal rounding rule;
- allocations never consume more than a lot's remaining quantity;
- sum of allocated cost plus remaining cost equals original known cost;
- unknown-cost quantities remain identifiable and yield `realized_gain_loss = null`, not zero;
- reversing a disposal restores the exact consumed lot state through compensating allocation facts.

- [ ] **Step 2: Run and verify failure**

```bash
cargo test --test portfolio_lots -- --nocapture
```

- [ ] **Step 3: Implement the pure lot engine**

The engine accepts an immutable snapshot and returns allocations/effects. It performs no I/O. Put rounding/remainder allocation in a named helper and preserve the final remainder on the last allocation so cost is conserved exactly.

- [ ] **Step 4: Run focused and randomized tests**

```bash
cargo test --test portfolio_lots -- --nocapture
```

- [ ] **Step 5: Commit**

```bash
git add src/contexts/portfolio/domain/lot.rs src/contexts/portfolio/domain/mod.rs tests/portfolio_lots.rs
git commit -m "feat(portfolio): add fifo and explicit lot allocation"
```

---

## Task 5: Add Portfolio repositories and unit of work

**Files:**

- Create: `src/contexts/portfolio/application/ports.rs`
- Create: `src/contexts/portfolio/infrastructure/repository.rs`
- Create: `src/contexts/portfolio/infrastructure/unit_of_work.rs`
- Create: `src/contexts/portfolio/infrastructure/mod.rs`
- Modify: `tests/portfolio_persistence.rs`

- [ ] **Step 1: Write failing transactionality/concurrency tests**

Verify one SQL transaction persists/locks the `portfolio.command_receipts` claim, Portfolio aggregate, lot effects, current projection, durable command result, audit metadata, and outbox event. Inject a failure before commit and assert none remain. Verify same-scope/key/same-hash replay returns the exact stored result with no second effect, same-scope/key/different-hash conflicts, and different scopes are independent. Run concurrent disposal attempts against the same position; exactly one succeeds when combined demand exceeds available quantity.

- [ ] **Step 2: Define aggregate-shaped ports**

Define repositories/UoW for Instrument, PortfolioAccount, PortfolioTransaction with lot state, PositionProjection, Valuation, a scoped command-receipt store, and outbox. Do not expose per-table `update_quantity` or `delete_transaction` methods.

- [ ] **Step 3: Implement SQLx adapters with stable lock order**

Lock `(user_id, portfolio_account_id, instrument_id)` position keys before loading lots. Use the shared idempotency request-hash contract and commit the terminal serialized result/HTTP status in `portfolio.command_receipts`. Posted facts are insert-only.

- [ ] **Step 4: Run focused tests**

```bash
SQLX_OFFLINE=true cargo test --test portfolio_persistence -- --nocapture
```

- [ ] **Step 5: Commit**

```bash
git add src/contexts/portfolio/application/ports.rs src/contexts/portfolio/infrastructure tests/portfolio_persistence.rs
git commit -m "feat(portfolio): persist portfolio aggregates atomically"
```

---

## Task 6: Implement task-oriented Portfolio command handlers

**Files:**

- Create: `src/contexts/portfolio/application/commands.rs`
- Create: `src/contexts/portfolio/application/handlers.rs`
- Create: `src/contexts/portfolio/application/mod.rs`
- Modify: `src/contexts/portfolio/public.rs`
- Modify: `tests/portfolio_domain.rs`
- Modify: `tests/portfolio_persistence.rs`

- [ ] **Step 1: Write failing handler tests**

Add tests for:

- `CreateManualOvdpInstrument`
- `OpenPortfolioAccount`
- `RecordOpeningPosition`
- `RecordPurchase`
- `RecordSale`
- `RecordCoupon`
- `RecordRedemption`
- `CorrectPosition`
- `ReversePortfolioTransaction`

Test ownership, archived state, currency, duplicate idempotency key/same hash, duplicate key/different hash, stale `expected_account_version`, stale `expected_position_version`, insufficient quantity, duplicate reversal, and correct event payload/version. First acquisition uses expected position version `0`; all later quantity/cost-affecting commands fence the version returned by the position read.

- [ ] **Step 2: Implement command handlers**

Handlers validate through aggregates, lock/load state, invoke the lot engine, persist all facts/projections/outbox atomically, and return stable result DTOs. A retry returns the original result including Portfolio transaction ID and processing status.

- [ ] **Step 3: Define the public boundary**

`public.rs` exposes opaque command/result/query types and versioned events. It must not expose SQLx repository traits, table rows, or mutable lot internals.

- [ ] **Step 4: Run tests and architecture guard**

```bash
cargo test --test portfolio_domain
SQLX_OFFLINE=true cargo test --test portfolio_persistence command_ -- --nocapture
cargo test --test context_boundaries -- --nocapture
```

- [ ] **Step 5: Commit**

```bash
git add src/contexts/portfolio/application src/contexts/portfolio/public.rs tests/portfolio_domain.rs tests/portfolio_persistence.rs
git commit -m "feat(portfolio): add portfolio application commands"
```

---

## Task 7: Add position and valuation projections

**Files:**

- Create: `src/contexts/portfolio/domain/valuation.rs`
- Create: `src/contexts/portfolio/infrastructure/projection.rs`
- Create: `src/contexts/portfolio/application/queries.rs`
- Modify: `src/contexts/portfolio/application/commands.rs`
- Modify: `src/contexts/portfolio/application/handlers.rs`
- Modify: `src/contexts/portfolio/public.rs`
- Modify: `tests/portfolio_persistence.rs`

- [ ] **Step 1: Write failing projection tests**

For each account/instrument derive:

- held quantity;
- known and unknown-cost quantity;
- remaining known cost basis;
- realized proceeds, allocated cost, fees, and realized gain/loss where known;
- most recent manual price/accrued-interest snapshot;
- market value and valuation `as_of`.

Valuation uses `quantity * (price_per_instrument + accrued_interest_per_instrument)` with explicit Decimal rounding at the currency boundary. Both inputs are absolute `Money` in the instrument currency for one bond; V1 does not accept an ambiguous percentage-of-face or yield quote. A UI may convert an explicitly labeled percent quote before submission, but the stored snapshot retains the absolute values, quote convention, source, and quote time and never changes a transaction or lot.

Also test the `RecordValuationSnapshot` command: owner/account/instrument/currency validation, positive price, non-negative accrued interest, required source/quote time, `Idempotency-Key` replay and payload conflict, atomic valuation/audit/idempotency/outbox/projection update, and no cash/Ledger process creation.

- [ ] **Step 2: Test append-only valuation and rebuild**

Reject update/delete of valuation facts. Delete only projection rows, rebuild from immutable Portfolio facts, and assert exact equality including ordering/checkpoint.

- [ ] **Step 3: Implement the valuation command, projections, and queries**

`RecordValuationSnapshot` appends the fact through the Portfolio UoW and updates the latest-valuation projection, audit, idempotency result, and outbox atomically. Queries return immutable DTOs for instruments, accounts, activity, positions, lots, and valuations. Include `as_of`, source, and missing-cost/missing-price states.

- [ ] **Step 4: Run tests**

```bash
SQLX_OFFLINE=true cargo test --test portfolio_persistence -- --nocapture
```

- [ ] **Step 5: Commit**

```bash
git add src/contexts/portfolio/domain/valuation.rs src/contexts/portfolio/infrastructure/projection.rs src/contexts/portfolio/application/commands.rs src/contexts/portfolio/application/handlers.rs src/contexts/portfolio/application/queries.rs src/contexts/portfolio/public.rs tests/portfolio_persistence.rs
git commit -m "feat(portfolio): project positions and manual valuations"
```

---

## Task 8: Coordinate optional Ledger cash settlement

**Files:**

- Create: `src/integration/process_managers/portfolio_cash_settlement.rs`
- Modify: `src/integration/process_managers/mod.rs`
- Modify: `src/contexts/portfolio/application/handlers.rs`
- Modify: `src/contexts/portfolio/public.rs`
- Create: `tests/portfolio_cash_settlement.rs`

- [ ] **Step 1: Write failing workflow tests with a fake Ledger façade**

Cover outgoing buy cash, incoming sale/coupon/redemption cash, no-cash transaction, wrong account/currency, Ledger business rejection, transient failure/retry, crash after Ledger commit, duplicate event delivery, Portfolio transaction reversal, completion-event replay, and both orderings of reversal versus an original settlement that is pending/in flight.

Assert:

- the integration adapter calls Ledger's typed `RecordCashControlSettlement` capability rather than accepting arbitrary postings from Portfolio;
- the idempotency key is derived from Portfolio transaction ID plus workflow version/action;
- Portfolio and Ledger journal IDs share a correlation ID;
- only one Ledger financial effect occurs;
- process state is `Pending`, `Posted`, `Retrying`, `Failed`, `CancelledNoFinancialEffect`, or `Reversed` with timestamps/error and optional journal/reversal IDs appropriate to that state;
- a Portfolio reversal requests the corresponding Ledger reversal exactly once;
- a reversal before the original cash effect atomically cancels that source operation, while a reversal after the original effect posts exactly one compensating journal;
- a stale/in-flight original worker cannot post after Ledger has durably accepted cancellation for the shared source-operation identity;
- selected Ledger account currency equals settlement currency in V1.

The closed Ledger recipe is debit hidden Portfolio settlement control / credit selected cash for an outgoing buy (including explicitly modeled cash fees), and debit cash / credit that control for incoming sale, coupon, or redemption proceeds. The control account is excluded from account/net-worth totals; Portfolio events—not the control posting—classify acquisition cost, proceeds, coupon income, fees, and realized gain/loss. Reversal posts the exact inverse through Ledger. This keeps the journal balanced without treating a security as cash or counting investment income/expense twice.

- [ ] **Step 2: Implement the durable process manager**

Persist/claim its inbox and state before invoking Ledger. On restart, retry any leased/expired non-terminal state. Serialize original/cancel/reverse actions by Portfolio transaction and workflow generation. The original uses Ledger's typed `RecordCashControlSettlement`; reversal uses `CancelOrReverseCashControlSettlement` with the same source-operation identity. Ledger's source-operation receipt is the final race arbiter: not-yet-posted becomes `CancelledNoFinancialEffect` with no fabricated journal ID, already-posted becomes `Reversed` with both journal/reversal references, and a late original returns the stored cancelled result. Publish distinct versioned cancellation-without-effect versus reversal completion events so API/Reporting cannot mislabel the outcome. After Ledger returns a durable result, persist the appropriate reference and publish the corresponding completion event.

- [ ] **Step 3: Keep Portfolio commits independent**

The Portfolio transaction is valid even when optional cash accounting is still pending or failed. Its API result includes accounting status and correlation ID. Do not hold a Portfolio SQL transaction open across the Ledger call.

- [ ] **Step 4: Run workflow tests**

```bash
cargo test --test portfolio_cash_settlement -- --nocapture
```

- [ ] **Step 5: Commit**

```bash
git add src/integration/process_managers/portfolio_cash_settlement.rs src/integration/process_managers/mod.rs src/contexts/portfolio/application/handlers.rs src/contexts/portfolio/public.rs tests/portfolio_cash_settlement.rs
git commit -m "feat(portfolio): coordinate ledger cash settlements"
```

---

## Task 9: Expose the Portfolio API

**Files:**

- Create: `src/contexts/portfolio/api/dto.rs`
- Create: `src/contexts/portfolio/api/handlers.rs`
- Create: `src/contexts/portfolio/api/routes.rs`
- Create: `src/contexts/portfolio/api/mod.rs`
- Modify: `src/contexts/portfolio/mod.rs`
- Modify: `src/contexts/mod.rs`
- Modify: `src/api/v2.rs`
- Modify: `src/bootstrap/v2.rs`
- Modify: `static/openapi.v2.json`
- Create: `tests/portfolio_api.rs`

- [ ] **Step 1: Write failing API contract tests**

Cover authentication/tenant isolation, decimal-string serialization, required `Idempotency-Key`, same-scope/key/same-canonical-hash durable response replay, same-scope/key/different-hash `409`, independent command scopes, `expected_version`, stable validation codes, and process status for:

```text
POST /portfolio-accounts
PATCH /portfolio-accounts/{id}
POST /portfolio-accounts/{id}/archive
POST /portfolio-accounts/{id}/restore
POST /instruments/ovdp
POST /portfolio-transactions
POST /portfolio-transactions/{id}/reversals
POST /valuations
GET  /portfolio-accounts
GET  /portfolio-accounts/{id}
GET  /portfolio-accounts/{id}/activity
GET  /instruments
GET  /instruments/{id}
GET  /portfolio-positions?portfolio_account_id={id}
GET  /valuations?portfolio_account_id={id}&instrument_id={id}
```

These exact reads provide account/instrument/position/activity/valuation detail required by the UI. Every POST/PATCH command requires `Idempotency-Key`. Account rename/archive/restore require body `expected_version` for `PortfolioAccount` and reject missing/stale values; account creation starts at version 1. `POST /portfolio-transactions` and its reversal route require `expected_account_version` to fence account archive/metadata changes and `expected_position_version` for the affected `(portfolio_account, instrument)` position (`0` means no position yet). The reversal locks/rechecks the original immutable transaction plus current position under that fence; duplicate-reversal uniqueness remains final protection. `POST /instruments/ovdp`, `POST /portfolio-accounts`, and append-only `POST /valuations` create new facts and therefore have no existing aggregate `expected_version`. Position reads return their current version. The Portfolio transaction write request uses a discriminated task payload, not arbitrary lot or Ledger postings.

- [ ] **Step 2: Implement DTO mapping and isolated router**

Keep `Money`, quantities, prices, face values, and accrued interest as decimal strings with explicit currencies/units. Return effective/recorded time, source, reversal, cash-accounting status, correlation, version, and `as_of`. Compose the routes only in `src/api/v2.rs`; the default router remains untouched until Phase 8.

- [ ] **Step 3: Update the parallel OpenAPI document**

Validate examples and error responses. Do not mount this router in the default legacy application; Phase 8 promotes the assembled V2 router.

- [ ] **Step 4: Run API/OpenAPI tests**

```bash
cargo test --test portfolio_api -- --nocapture
cargo test --test openapi_v2 -- --nocapture
```

- [ ] **Step 5: Commit**

```bash
git add src/contexts/portfolio/api src/contexts/portfolio/mod.rs src/contexts/mod.rs src/api/v2.rs src/bootstrap/v2.rs static/openapi.v2.json tests/portfolio_api.rs
git commit -m "feat(portfolio): expose manual ovdp api"
```

---

## Task 10: Feed Reporting without double counting

**Files:**

- Modify: `src/contexts/reporting/public.rs`
- Modify: `src/contexts/reporting/application/projectors.rs`
- Create: `src/contexts/reporting/infrastructure/portfolio_projection.rs`
- Modify: `src/contexts/reporting/infrastructure/mod.rs`
- Modify: `src/bootstrap/v2.rs`
- Create: `tests/reporting_portfolio.rs`

- [ ] **Step 1: Write failing Reporting tests**

Consume Portfolio transaction, reversal, position, valuation, cash-settlement-posted/reversed, and cash-settlement-cancelled-without-effect events through the Reporting inbox. Verify:

- cost/quantity/activity survives valuation changes;
- latest price is selected by `(quoted_at, event_sequence, id)`;
- stale/duplicate events do not regress a projection;
- cancellation before cash posting records terminal no-effect workflow history without creating a fake cash journal/reversal or changing value/net worth;
- Ledger hidden Portfolio settlement control accounts are excluded from account/net-worth totals;
- net worth adds Portfolio market value once and does not count optional cash settlement twice;
- missing valuation/cost is represented as incomplete, not zero;
- replay/rebuild equals the live projection.

- [ ] **Step 2: Implement the consumer/projection**

Use versioned public event DTOs only. Register the Portfolio event names/versions in Reporting's central projector dispatch, export the adapter from Reporting infrastructure, and wire that consumer in `bootstrap::v2` with the shared inbox/checkpoint runtime. Add a bootstrap assertion that publishing a Portfolio event through the normal dispatcher—not by calling the projection directly—updates Reporting. No SQL may query `portfolio.*` from Reporting or `ledger.*` from Portfolio.

- [ ] **Step 3: Run Reporting and architecture tests**

```bash
cargo test --test reporting_portfolio -- --nocapture
cargo test --test context_boundaries -- --nocapture
```

- [ ] **Step 4: Commit**

```bash
git add src/contexts/reporting/public.rs src/contexts/reporting/application/projectors.rs src/contexts/reporting/infrastructure/portfolio_projection.rs src/contexts/reporting/infrastructure/mod.rs src/bootstrap/v2.rs tests/reporting_portfolio.rs
git commit -m "feat(reporting): include portfolio valuation in net worth"
```

---

## Task 11: Phase verification and documentation

**Files:**

- Modify: `docs/superpowers/plans/2026-08-05-finance-v2-phase-7-portfolio-ovdp.md` — check completed tasks and record deviations
- Modify: operational/API docs created by earlier V2 phases if behavior differs

- [ ] **Step 1: Run the complete Phase 7 gate**

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --test portfolio_domain
cargo test --test portfolio_lots -- --nocapture
SQLX_OFFLINE=true cargo test --test portfolio_persistence -- --nocapture
cargo test --test portfolio_cash_settlement -- --nocapture
cargo test --test portfolio_api -- --nocapture
cargo test --test reporting_portfolio -- --nocapture
cargo test --test context_boundaries -- --nocapture
cargo test --test openapi_v2 -- --nocapture
cargo test
```

- [ ] **Step 2: Run an end-to-end scenario**

On a fresh V2 database: create UAH ОВДП instrument/account, record two purchase lots, sell across FIFO lots, post a coupon with optional cash, record valuation, redeem/reverse a transaction, rebuild projections, and assert exact Portfolio/Reporting/Ledger correlation.

- [ ] **Step 3: Confirm scope exclusions**

Search for broker clients, live pricing, tax calculations, automatic schedules, generic Portfolio posting endpoints, and direct foreign-context SQL. None should have been introduced.

- [ ] **Step 4: Commit phase-close documentation**

```bash
git add docs static/openapi.v2.json
git commit -m "docs(portfolio): close phase 7 verification"
```

## Verification commands

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --test portfolio_domain
cargo test --test portfolio_lots -- --nocapture
SQLX_OFFLINE=true cargo test --test portfolio_persistence -- --nocapture
cargo test --test portfolio_cash_settlement -- --nocapture
cargo test --test portfolio_api -- --nocapture
cargo test --test reporting_portfolio -- --nocapture
cargo test --test context_boundaries -- --nocapture
cargo test --test openapi_v2 -- --nocapture
cargo test
```

Task 11 is the canonical Phase 7 gate. The fresh-database scenario there is mandatory, not an optional manual demo.

## Commit boundaries

1. Portfolio schema/invariants.
2. Instrument and account aggregates.
3. Immutable activity model.
4. Lot engine.
5. SQLx UoW/persistence.
6. Application/public contracts.
7. Position/valuation projections.
8. Ledger cash-settlement process manager.
9. API/OpenAPI.
10. Reporting integration.
11. Verification/documentation.

Do not squash schema, lot arithmetic, process-manager reliability, and API work into one commit; each boundary is independently reviewable and revertible before Phase 8.

## Exit criteria

- [ ] All transaction kinds and reversals are immutable, tenant safe, idempotent, and auditable.
- [ ] Every Portfolio command uses `portfolio.command_receipts` atomically; scoped same-hash retries replay the durable result, different hashes conflict, and accounts/instruments/valuations are covered as well as transactions.
- [ ] FIFO and explicit allocations conserve quantity and known cost exactly under concurrency.
- [ ] Unknown historical cost remains explicit and never produces a fabricated gain/loss.
- [ ] Position and valuation projections rebuild exactly from facts.
- [ ] Optional Ledger cash settlement has one effect across crash/retry/replay and exposes status/correlation.
- [ ] Portfolio value never overwrites Ledger cash and Reporting does not double count the settlement control account.
- [ ] API money/quantity fields use decimal strings and no endpoint accepts arbitrary postings.
- [ ] Architecture, migration, OpenAPI, format, clippy, and full test gates pass.
- [ ] Default legacy runtime wiring remains unchanged until Phase 8.
