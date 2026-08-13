# Finance V2 Phase 2 — Ledger Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build and exhaustively test an immutable, tenant-safe, double-entry Ledger and its unversioned replacement account/transaction router in parallel, without changing the running legacy application or database.

**Architecture:** `LedgerAccount` and `JournalEntry` are the write aggregates. A journal commit writes its immutable postings, account balance projection, command receipt, audit record, and outbox events in one PostgreSQL transaction owned by a Ledger unit of work. Every currency represented in a journal balances to zero at commit. User-visible Asset/Liability accounts and system-controlled Income/Expense/Equity/FX-clearing accounts share the same posting model. Balance is a projection derived from postings, never an independently editable fact. “Correct balance” posts an explicit adjustment journal with before/target/delta/reason/actor. Transfers, fees, and FX legs are one command and one atomic journal.

**Tech Stack:** Rust 2024, PostgreSQL 16, sqlx 0.8, axum 0.8, tokio, rust_decimal, uuid, chrono, serde, thiserror, testcontainers

**Dependencies:** Finance V2 Phase 1 complete, including the guarded parallel migrator, shared kernel, context skeletons, concrete integration runtime, context-boundary test, and V2 migrations `0001`–`0002`.

---

## Hard boundaries and accounting conventions

- `Ledger` never imports another context's domain, repository, infrastructure, or DTO types and never names a concrete provider such as Monobank. It understands generic external provenance and narrowly typed accounting intents exposed by its own `public` facade; Banking, Recurring, Loans, Sharing, and Portfolio adapt their workflows to those contracts.
- The persisted source of truth is immutable journal entries and postings. `ledger.account_balances` is rebuildable and must equal posted activity.
- Posting amounts use debit-positive/credit-negative sign. For a journal and currency, `SUM(postings.signed_amount) = 0`.
- API `signed_balance` is the raw debit-positive sum. API `display_balance` applies normal sign: Asset/Expense `+1`; Liability/Income/Equity `-1`. A credit-card debt can therefore be displayed as a positive amount owed without corrupting the journal convention.
- User-managed accounts may be Asset or Liability. Income, Expense, Equity, and FX-clearing accounts are `authority = system` and cannot be opened, renamed, archived, or posted directly by an API caller.
- User-visible kinds are `cash`, `debit_card`, `credit_card`, `current`, `savings`, `jar`, `loan_payable`, and `loan_receivable`. Securities/ОВДП are **not** Ledger account kinds; they belong to Portfolio and later expose a valuation, not a scalar cash balance.
- Account authority is `manual`, `provider_observed`, or `system`; visibility is `user_visible` or `hidden`; lifecycle is `active` or `archived`; optimistic `version` starts at 1.
- Currency is immutable after account creation. Archive/restore retains history and may occur at non-zero balance. Archive blocks ordinary new user activity but never hides the balance and must still allow explicit reversal/correction/reconciliation workflows needed to preserve accounting truth.
- A posted journal and its postings are never updated or deleted. Reversal/replacement creates new journals with immutable relationship fields. Annotation edits are versioned and audited separately.
- At least two postings are required. Every posting account belongs to the same user and uses the posting currency.
- Same-currency transfer is at least two postings. Cross-currency transfer uses paired FX-clearing postings so each currency independently balances. Fees are additional postings in the fee currency inside the same journal.
- All financial POST endpoints require `Idempotency-Key`. Reusing a key with the same command fingerprint returns the stored result; reusing it with a different payload returns conflict.
- Every inbound monetary DTO is canonicalized through `reference_data::public::CurrencyCatalog` before its idempotency fingerprint is computed: the currency must be active/recognized and the decimal scale must not exceed that currency's minor-unit scale. `Money` then preserves the exact accepted Decimal; handlers never round an over-precise request implicitly.
- Aggregate metadata mutations require an explicit `expected_version` request field. Posted journal creation is serialized by idempotency and account locks, not by a mutable journal version.
- No `DELETE` endpoint exists for posted financial events. No `transactions` compatibility view exists.

## Parallel-build boundary

This phase never changes `src/main.rs`, `src/infrastructure/db.rs`, `src/infrastructure/test_db.rs`, `tests/common/mod.rs`, `tests/migrations.rs`, `src/api/routes.rs`, Docker, environment files, `DATABASE_URL`, or the public legacy API. Ledger tests use `v2_test_db` explicitly and invoke an isolated replacement V2 router directly. Phases 3–7 extend that parallel system. Phase 8 alone provisions the blank database and performs the irreversible runtime/API/default-migrator cutover.

---

## File Map

| File | Action | Responsibility |
|------|--------|----------------|
| `src/contexts/ledger/mod.rs` | Modify | Replace the Phase 1 skeleton with private layers; export `api` and `public` only |
| `src/contexts/ledger/domain/mod.rs` | Create | Domain exports |
| `src/contexts/ledger/domain/ids.rs` | Create | Ledger-owned account/journal/posting/annotation/reconciliation IDs |
| `src/contexts/ledger/domain/account.rs` | Create | Ledger account aggregate, nature/kind/authority/lifecycle/version |
| `src/contexts/ledger/domain/journal.rs` | Create | Immutable journal, postings, per-currency balancing |
| `src/contexts/ledger/domain/annotation.rs` | Create | Versioned mutable description/category/note/tags/budget-visibility aggregate |
| `src/contexts/ledger/domain/reconciliation.rs` | Create | Provider-neutral balance observation and approval case aggregate |
| `src/contexts/ledger/domain/error.rs` | Create | Typed invariant/conflict/not-found errors |
| `src/contexts/ledger/application/mod.rs` | Create | Ledger application exports |
| `src/contexts/ledger/application/ports.rs` | Create | Aggregate repositories, read queries, UoW, audit/outbox ports |
| `src/contexts/ledger/application/accounts.rs` | Create | Open/rename/archive account commands |
| `src/contexts/ledger/application/transactions.rs` | Create | Income/expense journal commands |
| `src/contexts/ledger/application/transfers.rs` | Create | Same-currency and FX transfer commands |
| `src/contexts/ledger/application/corrections.rs` | Create | Balance correction, reverse, replace commands |
| `src/contexts/ledger/application/annotations.rs` | Create | Versioned annotation commands |
| `src/contexts/ledger/application/reconciliation.rs` | Create | Observe/approve/dismiss reconciliation cases with balance-version fencing |
| `src/contexts/ledger/application/internal_commands.rs` | Create | Typed provider/sharing/loan/portfolio accounting contracts and builders |
| `src/contexts/ledger/application/queries.rs` | Create | Account, balance, journal, and activity read models |
| `src/contexts/ledger/infrastructure/mod.rs` | Create | PostgreSQL adapter exports |
| `src/contexts/ledger/infrastructure/rows.rs` | Create | Private SQL row mappings |
| `src/contexts/ledger/infrastructure/pg_unit_of_work.rs` | Create | One transaction for aggregates/projection/audit/outbox/receipt |
| `src/contexts/ledger/infrastructure/pg_repositories.rs` | Create | Transaction-bound aggregate persistence |
| `src/contexts/ledger/infrastructure/pg_queries.rs` | Create | Pool-backed read-only query adapter |
| `src/contexts/ledger/infrastructure/projection.rs` | Create | Incremental/rebuild/verify balance projection |
| `src/contexts/ledger/public.rs` | Modify | Replace the Phase 1 marker with provider-agnostic command/query contracts |
| `src/contexts/ledger/api/mod.rs` | Create | Ledger V2 API exports |
| `src/contexts/ledger/api/dto.rs` | Create | Decimal-string request/response DTOs |
| `src/contexts/ledger/api/handlers.rs` | Create | Auth-scoped command/query handlers |
| `src/contexts/ledger/api/routes.rs` | Create | Unversioned replacement Ledger routes |
| `src/infrastructure/migrations_v2/0003_ledger.sql` | Create | Ledger tables, indexes, deferred constraints, immutability guards |
| `src/api/v2.rs` | Modify | Add Ledger to the isolated supporting-context router; V2 tests only until Phase 8 |
| `src/bootstrap/mod.rs` | Modify | Export the extended parallel V2 composition without selecting it at runtime |
| `src/bootstrap/v2.rs` | Modify | Add Ledger to the existing supporting-context bootstrap for tests/future promotion |
| `src/api/mod.rs` | Modify | Export V2 composition |
| `src/api/v2_state.rs` | Create | Isolated V2 façade/read-composition state without legacy finance repositories |
| `src/contexts/mod.rs` | Modify | Export Ledger context |
| `src/lib.rs` | Modify | Compile the parallel V2 modules without selecting them at runtime |
| `tests/ledger_domain.rs` | Create | Pure accounting and aggregate invariants |
| `tests/ledger_persistence.rs` | Create | UoW, database constraints, idempotency, projection invariants |
| `tests/ledger_concurrency.rs` | Create | Concurrent commands and lock-order regression tests |
| `tests/ledger_public_contracts.rs` | Create | Stable typed contracts for later process managers |
| `tests/ledger_api_v2.rs` | Create | Isolated breaking API behavior and visibility |
| `tests/openapi_v2.rs` | Modify | Extend parallel OpenAPI/route expectations with Ledger operations |
| `static/openapi.v2.json` | Modify | Extend the exact supporting-context manifest with the replacement financial API |

Legacy source files and runtime wiring remain untouched. No Phase 2 commit may remove, construct differently, or reroute `SqliteAccountRepository`, `SqliteTransactionRepository`, `AccountService`, `TransactionService`, `MonobankService`, `MatchChargesUseCase`, or `StatsRepository`; Phase 8 removes their active wiring.

---

## Task 1: Model Ledger accounts and system account policy

**Files:**
- Create: `src/contexts/ledger/domain/{mod.rs,ids.rs,account.rs,error.rs}`
- Modify: `src/contexts/ledger/mod.rs`
- Modify: `src/contexts/mod.rs`
- Create: `tests/ledger_domain.rs`

- [x] **Step 1 — RED: write account aggregate tests**

Cover:

```text
manual Cash/Asset and DebitCard/Asset creation
provider-owned account creation through an internal command
CreditCard/Liability and LoanPayable/Liability compatibility
rejection of invalid nature/kind combinations
system-account creation unavailable to public callers
hidden account visibility unavailable to public callers
rename with expected version
stale version conflict
archived account rejects ordinary new income/expense/transfer postings
archive/restore at non-zero balance retains visible balance and history
archived account blocks ordinary new activity but permits explicit corrective/reversal flows
currency change rejected even before first posting
```

Account construction accepts a `Clock`; it never calls `Utc::now()` internally.

- [x] **Step 2 — Run the tests to verify RED**

Run: `cargo test --test ledger_domain account`

Expected: FAIL because the Ledger domain does not exist.

- [x] **Step 3 — GREEN: implement `LedgerAccount` and policy types**

Model `AccountNature`, `AccountKind`, `AccountAuthority`, `AccountVisibility`, `AccountLifecycle`, and `AccountVersion` as enums/newtypes rather than arbitrary strings. Keep provider product metadata out of this aggregate.

System accounts include uncategorized income, uncategorized expense, balance-adjustment equity, opening-balance equity, and per-currency FX clearing. They are created lazily by an internal application service in the same UoW that needs them.

- [x] **Step 4 — Run focused tests**

Run: `cargo test --test ledger_domain account`

Expected: PASS.

- [x] **Step 5 — REFACTOR: constrain the public constructors**

Make invalid authority/nature/kind combinations unrepresentable through public APIs. There must be no `set_balance`, `adjust_balance`, `delete`, or provider name in `domain/account.rs`.

- [x] **Step 6 — Commit boundary**

```bash
git add src/contexts/ledger src/contexts/mod.rs tests/ledger_domain.rs
git commit -m "feat(ledger): model account aggregate and system policy"
```

---

## Task 2: Model immutable journals, annotations, and reconciliation cases

**Files:**
- Create: `src/contexts/ledger/domain/journal.rs`
- Create: `src/contexts/ledger/domain/annotation.rs`
- Create: `src/contexts/ledger/domain/reconciliation.rs`
- Modify: `src/contexts/ledger/domain/mod.rs`
- Modify: `tests/ledger_domain.rs`

- [x] **Step 1 — RED: write journal invariant tests**

Test rejection of zero/one posting, a zero posting, an unbalanced currency, mixed users, currency/account mismatch, and an archived account for an ordinary new financial purpose. Explicitly prove that a reversal, correction, or approved-reconciliation purpose may post to an archived account so historical truth can still be repaired without restoring it for ordinary spending. Test balanced two-posting income/expense, multi-posting fee, and two-currency FX journals. Verify posting order and IDs are deterministic inside the aggregate.

Test immutable relation semantics for:

```text
reverses_transaction_id
corrects_transaction_id
replaces_transaction_id
```

A relation is set at construction and never patched onto an existing journal. Test annotations separately: description, category, note, tags, and budget-visibility updates require expected version and create an audit event, but never alter postings. Tags use a bounded, normalized, duplicate-free value set; budget visibility is explicit rather than inferred from category.

Test `ReconciliationCase` separately: a provider-neutral external balance observation captures source reference, observed/recorded timestamps, provider-reported and available balances, current Ledger display balance, current account-balance version, and delta. Zero delta creates a terminal `matched` case/audit fact; non-zero delta creates `pending`. Approval requires both expected case version and the captured balance version; dismissal is versioned; no domain method directly sets account balance.

- [x] **Step 2 — Run the tests to verify RED**

Run: `cargo test --test ledger_domain -- --nocapture`

Expected: FAIL because journal, annotation, and reconciliation types are absent.

- [x] **Step 3 — GREEN: implement journal construction and validation**

`JournalEntry::post` takes actor, source, correlation ID, idempotency key, occurred/recorded timestamps, relation fields, and at least two postings. Validate `SUM(signed_amount) == 0` independently for every `CurrencyCode`.

Source is a provider-neutral enum/value such as `manual`, `import`, `system`, or `correction`; it is not `Monobank`.

- [x] **Step 4 — Run focused tests**

Run: `cargo test --test ledger_domain -- --nocapture`

Expected: PASS.

- [x] **Step 5 — REFACTOR: remove mutable journal methods**

The journal exposes no edit/delete/status-setter methods. Reversal and replacement builders return new journal commands. Annotation is the only mutable user-facing transaction metadata aggregate.

- [x] **Step 6 — Commit boundary**

```bash
git add src/contexts/ledger/domain tests/ledger_domain.rs
git commit -m "feat(ledger): enforce immutable balanced journals"
```

---

## Task 3: Create the strict Ledger schema

**Files:**
- Create: `src/infrastructure/migrations_v2/0003_ledger.sql`
- Modify: `tests/v2_migrations.rs`
- Create: `tests/ledger_persistence.rs`

- [x] **Step 1 — RED: write database invariant tests before SQL**

Test direct SQL attempts for:

- one-posting and unbalanced journal commit rejection;
- balanced multi-currency journal acceptance;
- cross-user account/posting rejection;
- account/posting currency mismatch rejection;
- update/delete rejection for journals and postings;
- account currency change rejection immediately after creation;
- no hard delete of an account with history;
- duplicate `(user_id, command_name, idempotency_key)` rejection;
- duplicate relation/reversal where policy allows only one active reversal; and
- tenant-safe reconciliation cases, legal status transitions, and positive balance-projection versions;
- `NUMERIC(28,8)` overflow/scale constraints.

- [x] **Step 2 — Run the tests to verify RED**

Run: `cargo test --test ledger_persistence schema`

Expected: FAIL because migration `0003` is absent.

- [x] **Step 3 — GREEN: create schema in dependency order**

Create, in this order:

```text
ledger.accounts
ledger.journal_entries
ledger.postings
ledger.transaction_annotations
ledger.balance_correction_details
ledger.reconciliation_cases
ledger.command_receipts
ledger.account_balances
ledger.audit_events
```

Use `UNIQUE (id, user_id)` and, where needed, `UNIQUE (id, user_id, currency)` so composite foreign keys enforce tenant and currency ownership. Use `TIMESTAMPTZ`, ordered `BIGSERIAL`/identity sequences for stable pagination, and `NUMERIC(28,8)` compatible with `rust_decimal`. The balance projection carries a monotonically increasing `version`; reconciliation cases persist the observed balance, captured Ledger balance/version, delta, source reference, status/version, approval journal, reason, and actor without referencing Banking tables.

Add DEFERRABLE constraint triggers that verify at least two postings and a zero sum per `(journal_entry_id, currency)` at commit. Add guards that reject UPDATE/DELETE on journal entries, postings, correction details, and audit events. Add normal indexes during this blank-database migration; do not use `CONCURRENTLY` or staged validation.

- [x] **Step 4 — Run migration and persistence tests**

Run:

```bash
cargo test --test v2_migrations
cargo test --test ledger_persistence schema
```

Expected: PASS.

- [x] **Step 5 — REFACTOR: inspect every foreign key and trigger**

Verify no Ledger DDL references `banking`, `mail`, `subscriptions`, or a provider. The only cross-schema references allowed are immutable currency codes/reference IDs and integration outbox written by the UoW.

- [x] **Step 6 — Commit boundary**

```bash
git add src/infrastructure/migrations_v2/0003_ledger.sql tests/v2_migrations.rs tests/ledger_persistence.rs
git commit -m "feat(db): add strict double-entry Ledger schema"
```

---

## Task 4: Implement the transaction-bound Ledger unit of work

**Files:**
- Create: `src/contexts/ledger/application/{mod.rs,ports.rs}`
- Create: `src/contexts/ledger/infrastructure/{mod.rs,rows.rs,pg_unit_of_work.rs,pg_repositories.rs}`
- Modify: `src/contexts/ledger/mod.rs`
- Modify: `tests/ledger_persistence.rs`

- [ ] **Step 1 — RED: test atomic success and rollback**

Write integration tests that inject a failure after each stage: journal insert, postings, balance projection, command receipt, audit event, and outbox append. After rollback, assert none of those tables changed. Also prove an account row lock and its repositories use the exact same SQLx transaction.

- [ ] **Step 2 — Run the tests to verify RED**

Run: `cargo test --test ledger_persistence unit_of_work`

Expected: FAIL because no Ledger UoW exists.

- [ ] **Step 3 — GREEN: implement one UoW per command**

Expose aggregate-specific ports, not a generic CRUD repository:

```rust
pub trait LedgerUnitOfWork {
    type Tx<'a>: LedgerAccountStore + JournalStore + ProjectionStore
        + CommandReceiptStore + AuditStore + OutboxWriter;
    async fn begin(&self) -> Result<Self::Tx<'_>, LedgerError>;
}
```

The concrete `PgLedgerUnitOfWork` owns `sqlx::Transaction<'_, Postgres>`. Stores created from it borrow that transaction. There is no pool-backed `adjust_balance`, `set_balance`, standalone posting insert, or journal delete.

- [ ] **Step 4 — Run focused tests**

Run: `cargo test --test ledger_persistence unit_of_work`

Expected: PASS.

- [ ] **Step 5 — REFACTOR: separate writes from queries**

Do not add list/report methods to aggregate stores. Task 9 creates a pool-backed query adapter that is incapable of mutation.

- [ ] **Step 6 — Commit boundary**

```bash
git add src/contexts/ledger/application src/contexts/ledger/infrastructure src/contexts/ledger/mod.rs tests/ledger_persistence.rs
git commit -m "feat(ledger): add transactional aggregate unit of work"
```

---

## Task 5: Implement account commands and opening balances

**Files:**
- Create: `src/contexts/ledger/application/accounts.rs`
- Modify: `src/contexts/ledger/public.rs`
- Modify: `src/contexts/ledger/application/mod.rs`
- Modify: `tests/ledger_persistence.rs`

- [ ] **Step 1 — RED: write command tests**

Cover open at zero; Asset and Liability accounts opened with positive and negative explicit display balances; same-key replay; same-key/different-payload conflict; cross-user invisibility; stale rename; archive/restore with optimistic version at zero and non-zero balance; retained activity after archive; and ordinary-post rejection while archived. An opening balance must create an immutable journal against system opening-balance equity; it must not seed the projection directly. In particular, a positive asset opening debits the asset, while a positive amount owed credits the liability, and both normalize to the requested display balance.

- [ ] **Step 2 — Run the tests to verify RED**

Run: `cargo test --test ledger_persistence account_command`

Expected: FAIL because account command handlers do not exist.

- [ ] **Step 3 — GREEN: implement account command handlers**

For non-zero opening balance, translate the requested display balance through account nature, create/lock the system equity account in the same currency, post the two legs, update projection, write an account-opened audit event and journal event, save the command receipt, then commit. Normalize no business values; fingerprint the canonical command payload to detect mismatched replay.

- [ ] **Step 4 — Run focused tests**

Run: `cargo test --test ledger_persistence account_command`

Expected: PASS.

- [ ] **Step 5 — REFACTOR: publish a provider-neutral facade**

`ledger/public.rs` exposes commands such as `OpenAccount`, `RenameAccount`, and `ArchiveAccount`, plus result DTOs. Provider-owned account creation is an authenticated internal capability, not a public HTTP DTO and not a Monobank-specific method.

- [ ] **Step 6 — Commit boundary**

```bash
git add src/contexts/ledger/application/accounts.rs src/contexts/ledger/application/mod.rs src/contexts/ledger/public.rs tests/ledger_persistence.rs
git commit -m "feat(ledger): implement idempotent account commands"
```

---

## Task 6: Implement manual income and expense transactions

**Files:**
- Create: `src/contexts/ledger/application/transactions.rs`
- Modify: `src/contexts/ledger/application/mod.rs`
- Modify: `src/contexts/ledger/public.rs`
- Modify: `tests/ledger_persistence.rs`

- [ ] **Step 1 — RED: write transaction command tests**

Test income and expense against both Asset and Liability user accounts, the correct system counter-account, exact decimal amounts, category annotation IDs, occurred-at versus recorded-at, archived/wrong-user account rejection, idempotent replay, rollback on outbox failure, and projection equality after each post. Charging an expense to a credit-card liability must increase displayed debt; recording income against that liability must reduce it. A merchant refund is represented by reversal/replacement or a separately typed reclassification, never silently relabeled as income. Inject `classification::public` and prove an active same-user category succeeds while an archived, missing, or other-user category is rejected before the UoW commits.

Amounts in public commands are positive `Money`; direction determines journal signs. Reject zero/negative request amounts at the boundary instead of relying on sign tricks.

- [ ] **Step 2 — Run the tests to verify RED**

Run: `cargo test --test ledger_persistence manual_transaction`

Expected: FAIL because the use case does not exist.

- [ ] **Step 3 — GREEN: translate intent to a balanced journal**

Expense posts debit to the system expense account and credit to the selected Asset or Liability account; the shared sign-normalization rule makes this reduce an asset or increase a liability. Income posts debit to the selected Asset or Liability account and credit to system income; it increases an asset or reduces a liability. Validate category ownership/active lifecycle through `classification::public::CategoryCatalog`, then persist only its typed ID/snapshot in the annotation aggregate; Ledger never imports Classification internals or queries its tables.

- [ ] **Step 4 — Run focused tests**

Run: `cargo test --test ledger_persistence manual_transaction`

Expected: PASS.

- [ ] **Step 5 — REFACTOR: centralize journal commit orchestration**

Extract one internal commit pipeline used by manual transactions, transfers, corrections, and future provider imports. The pipeline must not accept an already-mutated projection.

- [ ] **Step 6 — Commit boundary**

```bash
git add src/contexts/ledger/application/transactions.rs src/contexts/ledger/application/mod.rs src/contexts/ledger/public.rs tests/ledger_persistence.rs
git commit -m "feat(ledger): post manual income and expense journals"
```

---

## Task 7: Implement atomic transfers, fees, and FX

**Files:**
- Create: `src/contexts/ledger/application/transfers.rs`
- Modify: `src/contexts/ledger/application/mod.rs`
- Modify: `src/contexts/ledger/public.rs`
- Modify: `tests/ledger_domain.rs`
- Modify: `tests/ledger_persistence.rs`
- Create: `tests/ledger_concurrency.rs`

- [ ] **Step 1 — RED: write transfer and deadlock tests**

Cover same-account rejection, cross-user rejection, Asset→Asset transfer, Asset→Liability card payment, Liability→Asset cash advance, Liability→Liability balance transfer, source-currency fee, target-currency fee, cross-currency four-leg FX transfer, explicit source/target amounts, recorded implied rate, idempotent replay, and full rollback if any leg fails. Assert both raw posting signs and normalized display-balance effects for every account-nature pairing.

Start simultaneous A→B and B→A transfers. The test must have a timeout and prove they complete without deadlock and without projection drift.

- [ ] **Step 2 — Run the tests to verify RED**

Run:

```bash
cargo test --test ledger_persistence transfer
cargo test --test ledger_concurrency opposing_transfers
```

Expected: FAIL because transfer orchestration is absent.

- [ ] **Step 3 — GREEN: build one journal and lock deterministically**

Lock all affected account/projection rows in sorted `LedgerAccountId` order. For FX, use a system FX-clearing account in each currency so each currency sums to zero. Record both amounts and the implied rate as immutable transaction facts. Post fees to a system expense account in the fee currency.

- [ ] **Step 4 — Run focused tests repeatedly**

Run:

```bash
cargo test --test ledger_persistence transfer
for i in 1 2 3 4 5; do cargo test --test ledger_concurrency opposing_transfers || exit 1; done
```

Expected: PASS five times without timeout, deadlock, or drift.

- [ ] **Step 5 — REFACTOR: keep FX pricing out of Ledger**

Ledger records the amounts/rate supplied and validates arithmetic; it does not fetch NBU/provider rates. Pricing policy belongs to a caller or future FX context.

- [ ] **Step 6 — Commit boundary**

```bash
git add src/contexts/ledger/application/transfers.rs src/contexts/ledger/application/mod.rs src/contexts/ledger/public.rs tests/ledger_domain.rs tests/ledger_persistence.rs tests/ledger_concurrency.rs
git commit -m "feat(ledger): post atomic transfers with fees and FX"
```

---

## Task 8: Implement visible corrections, reversal, replacement, and annotation edits

**Files:**
- Create: `src/contexts/ledger/application/corrections.rs`
- Create: `src/contexts/ledger/application/annotations.rs`
- Modify: `src/contexts/ledger/application/mod.rs`
- Modify: `src/contexts/ledger/public.rs`
- Modify: `tests/ledger_persistence.rs`

- [ ] **Step 1 — RED: write immutable-change tests**

Test that balance correction requires the expected account-balance version, locks the account projection, reads current display balance, computes delta, and records before/target/delta/reason/actor/observation time in an immutable detail row. An intervening posting causes a stale-version conflict with no effect. A zero-delta manual correction is rejected rather than writing noise. Test positive and negative Asset/Liability corrections.

Test exact negating reversal, one-reversal policy/idempotent replay, replacement as reversal plus replacement entry in one UoW, cross-user rejection, and unchanged original rows. Test description, note, normalized tags, budget visibility, and category annotation edits with expected version, audit visibility, and no posting/projection change. Category changes must revalidate same-user/active status through `classification::public`; archived and cross-user IDs are rejected without querying Classification SQL.

- [ ] **Step 2 — Run the tests to verify RED**

Run: `cargo test --test ledger_persistence -- --nocapture`

Expected: FAIL because these commands do not exist.

- [ ] **Step 3 — GREEN: implement new-event-only changes**

Correction computes `display_delta = target - current_display`, converts it to the raw debit-positive posting amount with `signed_posting_delta = display_delta * account.normal_sign()`, and posts that amount against balance-adjustment equity. This makes increasing displayed liability debt a credit and reducing it a debit. Reversal copies every original posting with the opposite sign and points `reverses_transaction_id` to the original. Replacement creates both the reversal and new balanced journal atomically, linked by correlation ID. Annotation mutation writes its own audit event and increments annotation version.

- [ ] **Step 4 — Run focused tests**

Run: `cargo test --test ledger_persistence -- --nocapture`

Expected: PASS.

- [ ] **Step 5 — REFACTOR: remove all mutation shortcuts**

Run:

```bash
rg -n "set_balance|adjust_balance|delete_transaction|UPDATE ledger\.journal|DELETE FROM ledger\.(journal|postings)" src/contexts/ledger
```

Expected: no production matches.

- [ ] **Step 6 — Commit boundary**

```bash
git add src/contexts/ledger/application/corrections.rs src/contexts/ledger/application/annotations.rs src/contexts/ledger/application/mod.rs src/contexts/ledger/public.rs tests/ledger_persistence.rs
git commit -m "feat(ledger): make financial corrections immutable and visible"
```

---

## Task 9: Build read models and prove projection correctness

**Files:**
- Create: `src/contexts/ledger/application/queries.rs`
- Create: `src/contexts/ledger/infrastructure/pg_queries.rs`
- Create: `src/contexts/ledger/infrastructure/projection.rs`
- Modify: `src/contexts/ledger/infrastructure/mod.rs`
- Modify: `src/contexts/ledger/public.rs`
- Modify: `tests/ledger_persistence.rs`
- Modify: `tests/ledger_concurrency.rs`

- [ ] **Step 1 — RED: write query and rebuild tests**

Test account lists/balances, stable cursor pagination by `(occurred_at, ledger_sequence)`, account activity, journal detail with postings/relations/annotation/source/actor, archived-account history, and cross-user invisibility.

Corrupt a projection in a test transaction, verify `verify_projection` detects exact account deltas, run `rebuild_projection`, and assert:

```text
account_balances.signed_balance
  == SUM(ledger.postings.signed_amount) for committed journals
```

Run concurrent manual posts/corrections/transfers and assert the same invariant afterward.

- [ ] **Step 2 — Run the tests to verify RED**

Run:

```bash
cargo test --test ledger_persistence -- --nocapture
cargo test --test ledger_concurrency projection_never_drifts
```

Expected: FAIL because query/projection adapters do not exist.

- [ ] **Step 3 — GREEN: implement read-only SQL and rebuild tooling**

`PgLedgerQueries` owns a pool and may only issue `SELECT`. Projection write/rebuild functions remain an operational adapter, unavailable through HTTP handlers. Stable ordering always includes the monotonic ledger sequence as a tie-breaker.

- [ ] **Step 4 — Run focused tests**

Run:

```bash
cargo test --test ledger_persistence -- --nocapture
cargo test --test ledger_concurrency
```

Expected: PASS.

- [ ] **Step 5 — REFACTOR: remove statistics from Ledger queries**

Net-worth charts, category rollups, and multi-currency valuations belong to Reporting. Ledger queries expose accounting facts and current per-account balances only.

- [ ] **Step 6 — Commit boundary**

```bash
git add src/contexts/ledger/application/queries.rs src/contexts/ledger/infrastructure/pg_queries.rs src/contexts/ledger/infrastructure/projection.rs src/contexts/ledger/infrastructure/mod.rs src/contexts/ledger/public.rs tests/ledger_persistence.rs tests/ledger_concurrency.rs
git commit -m "feat(ledger): add auditable queries and projection verification"
```

---

## Task 10: Build the unversioned replacement Ledger API in isolation

**Files:**
- Create: `src/contexts/ledger/api/{mod.rs,dto.rs,handlers.rs,routes.rs}`
- Modify: `src/api/v2.rs`
- Modify: `src/bootstrap/{mod.rs,v2.rs}`
- Modify: `static/openapi.v2.json`
- Modify: `src/api/mod.rs`
- Create: `src/api/v2_state.rs`
- Create: `tests/ledger_api_v2.rs`
- Modify: `tests/openapi_v2.rs`

- [ ] **Step 1 — RED: write the HTTP contract tests**

Cover authenticated tenant isolation, decimal-string parsing/serialization, unknown/inactive currency rejection, over-minor-unit-scale rejection without rounding, currency mismatch validation, missing/oversized idempotency key, same-key replay, mismatched replay conflict, missing/stale `expected_version`, error-code stability, and no financial `DELETE` routes. Prove canonical validated Money—not raw JSON spelling—is used consistently for command hashing and execution.

Required routes:

```text
POST   /accounts
GET    /accounts
GET    /accounts/{id}
PATCH  /accounts/{id}
POST   /accounts/{id}/archive
POST   /accounts/{id}/restore
GET    /accounts/{id}/activity
POST   /transactions
GET    /transactions
GET    /transactions/{id}
PATCH  /transactions/{id}/annotation
POST   /transactions/{id}/reversals
POST   /transactions/{id}/replacements
POST   /transfers
POST   /accounts/{id}/balance-corrections
```

Responses expose source, occurred/recorded timestamps, actor, correlation, idempotent replay flag, postings/effect, reversal/correction/replacement relations, signed/display balances, and annotation version. Account reads reserve `provider_reported`, `available`, and `reconciliation_difference` as nullable fields alongside Ledger balance, currency, version, and `as_of`; Phase 2 always returns those provider fields as `null`, never copies the Ledger value into them.

- [ ] **Step 2 — Run the tests to verify RED**

Run: `cargo test --test ledger_api_v2`

Expected: FAIL/404 because the isolated replacement routes do not exist.

- [ ] **Step 3 — GREEN: implement thin handlers and exact DTOs**

Handlers derive `UserId` only from authenticated JWT state, parse `Idempotency-Key` and body `expected_version`, validate/canonicalize every monetary input through `reference_data::public::CurrencyCatalog`, call the Ledger public facade, and map typed errors. They contain no SQL, balance arithmetic, or implicit rounding. Amounts are JSON strings plus explicit currency codes. `bootstrap::v2` constructs the V2 Ledger, Reference Data, Classification, Preferences, auth, and isolated router only from Phase 1's `VerifiedV2Pool`; it starts no workers and is never called by `main.rs` in this phase.

- [ ] **Step 4 — Run focused API and OpenAPI checks**

Run:

```bash
cargo test --test ledger_api_v2
cargo test --test openapi_v2
jq empty static/openapi.v2.json
```

Expected: PASS.

- [ ] **Step 5 — REFACTOR: compare route tests to OpenAPI**

Every route, status, required header, enum, and decimal-string schema must agree. OpenAPI must not advertise legacy account deletion, transaction deletion, direct balance update, or unauthenticated financial endpoints.

- [ ] **Step 6 — Commit boundary**

```bash
git add src/contexts/ledger/api src/api/v2.rs src/api/v2_state.rs src/api/mod.rs src/bootstrap src/lib.rs tests/ledger_api_v2.rs tests/openapi_v2.rs static/openapi.v2.json
git commit -m "feat(api): build isolated Finance V2 Ledger endpoints"
```

---

## Task 11: Implement provider-neutral balance reconciliation

**Files:**
- Create: `src/contexts/ledger/application/reconciliation.rs`
- Modify: `src/contexts/ledger/application/mod.rs`
- Modify: `src/contexts/ledger/public.rs`
- Modify: `src/contexts/ledger/api/{dto.rs,handlers.rs,routes.rs}`
- Modify: `static/openapi.v2.json`
- Modify: `tests/ledger_persistence.rs`
- Modify: `tests/ledger_api_v2.rs`

- [ ] **Step 1 — RED: write observation and approval tests**

Exercise the provider-neutral `ObserveProviderBalance` public command with a typed source-stream reference and total observation order `(observed_at, source_sequence, observation_id)`. A zero-delta observation must create a terminal `matched` reconciliation case plus audit/outbox facts and no journal entry. A non-zero observation creates `pending` with captured account balance/projection version. Approve must require body `expected_version` for the case and the captured balance version, then atomically post the visible balance-correction entry and mark the case approved.

Post an intervening transaction between observe and approve; approval must return typed `StaleObservedBalance`, mark/recompute only through an explicit refreshed observation, and make no journal/projection change. Cover duplicate observation idempotency, a newer observation superseding/refreshing a pending case with a new case version/audit fact, an older observation delivered after the newer one, and concurrent older/newer delivery before approval. The older fact remains linkable/audited but returns `IgnoredOlderObservation` and cannot regress or become the approvable active case. Approval of a superseded case is rejected. Also cover archived-account corrective approval, cross-user invisibility, dismiss, and retry of already-approved cases.

- [ ] **Step 2 — Run the tests to verify RED**

Run: `cargo test --test ledger_persistence reconciliation`

Expected: FAIL because reconciliation commands do not exist.

- [ ] **Step 3 — GREEN: implement observe/approve/dismiss in one UoW**

Observation never mutates a balance. Lock a reconciliation-stream row keyed by `(user, account, source_kind, source_stream_id)` and advance its latest tuple only when the incoming `(observed_at, source_sequence, observation_id)` is greater. Persist older deliveries as ignored/link history without changing the active case; mark a prior pending case `superseded` when a newer fact replaces it. Approval re-locks both stream and projection, proves the case is still latest/active, and compares its captured balance version before constructing the same correction journal path used by manual corrections. Persist before/target/delta/reason/actor and source observation reference. Emit case-observed/matched/superseded/ignored/approved/dismissed events transactionally.

- [ ] **Step 4 — Add and test the isolated API**

Add:

```text
GET  /reconciliations
GET  /reconciliations/{id}
POST /reconciliations/{id}/approve
POST /reconciliations/{id}/dismiss
```

Run:

```bash
cargo test --test ledger_persistence reconciliation
cargo test --test ledger_api_v2 reconciliation
```

Expected: PASS, including stale-balance-version error mapping and zero-delta visibility.

- [ ] **Step 5 — REFACTOR: keep provider observations outside Ledger storage**

Ledger stores only the typed source reference plus normalized reported/available `Money` needed for the reconciliation decision. Credit limits, statement-running balance, raw/provider-specific fields, and payload provenance remain in Banking. There is no provider token, resource metadata, or Banking foreign key in Ledger.

- [ ] **Step 6 — Commit boundary**

```bash
git add src/contexts/ledger/application/reconciliation.rs src/contexts/ledger/application/mod.rs src/contexts/ledger/public.rs src/contexts/ledger/api static/openapi.v2.json tests/ledger_persistence.rs tests/ledger_api_v2.rs
git commit -m "feat(ledger): add version-fenced balance reconciliation"
```

---

## Task 12: Freeze typed commands for later context process managers

**Files:**
- Create: `src/contexts/ledger/application/internal_commands.rs`
- Modify: `src/contexts/ledger/application/mod.rs`
- Modify: `src/contexts/ledger/public.rs`
- Create: `tests/ledger_public_contracts.rs`
- Modify: `tests/ledger_persistence.rs`

- [ ] **Step 1 — RED: write compile-time and accounting contract tests**

Freeze provider-neutral typed commands/results needed by later process managers:

```text
ImportProviderTransaction / TransitionProviderTransactionState / ReverseProviderTransaction
ObserveProviderBalance
ReclassifyExpenseToReceivableOrPayable / SettleReceivableOrPayable
RecordExpenseAndControlBalances
RecordPrincipalDisbursement / RecordPrincipalRepayment / RecordInterestAndFee
WriteOffLiabilityOrReceivable
EnsureTypedControlAccount / RecordCashControlSettlement / CancelOrReverseCashControlSettlement
```

Tests must show commands carry `UserId`, a typed external/domain reference, correlation/causation, `IdempotencyKey`, occurred-at, `Money`, and only the specific user/control account IDs required. Callers cannot submit arbitrary signed postings or select system income/equity accounts. Results return entry ID, effect, projection versions, replay flag, and outbox correlation needed by durable process state.

`EnsureTypedControlAccount` accepts a closed Ledger-owned role—not a free-form account nature/name—and an opaque subject reference where cardinality requires it. The initial roles cover external-subject Receivable/Payable, accrued Interest/Fee Receivable/Payable, and Portfolio cash clearing. This supports per-contact and per-loan control accounts while keeping provider/context DTOs outside Ledger and preventing callers from provisioning arbitrary system accounts.

`RecordExpenseAndControlBalances` is a closed recipe for one or more selected cash/card credits plus current-user Expense, typed Receivable, and typed Payable legs. It validates `expense + receivables = cash contributions + payables` in one currency and does not accept a generic posting list. `WriteOffLiabilityOrReceivable` requires a typed principal/accrual component, direction, reason, and the owning control account; Ledger selects the allowed bad-debt or forgiveness counter-account. `RecordCashControlSettlement` and `CancelOrReverseCashControlSettlement` share one opaque source-operation identity: under one Ledger lock/receipt, cancel wins before posting, while a cancellation received after posting creates exactly one reversal. A late original command after cancellation returns the stored cancelled result and cannot post. Contract tests freeze these recipes for Sharing, Loans, and Portfolio before their phases begin.

Add golden JSON/round-trip tests for versioned `LedgerEventV1` facts consumed later: account lifecycle changed, entry posted/reversed/replaced, annotation changed, balance changed, reconciliation observed/matched/superseded/ignored-older/approved/dismissed/stale, and internal accounting command posted/failed. Every event includes schema version, user, sequence, correlation/causation, occurred/recorded time, and minimum typed IDs/money effects; it contains no raw provider payload.

- [ ] **Step 2 — Run the contract tests to verify RED**

Run: `cargo test --test ledger_public_contracts`

Expected: FAIL because the internal command contracts/builders do not exist.

- [ ] **Step 3 — GREEN: implement controlled journal builders**

Translate each intent through the same validated journal/UoW pipeline. External import/state/reversal remains provider-neutral and idempotent by source/resource/event/revision. Receivable/payable reclassification, principal/interest, and cash-control settlement encode Ledger accounting intent without importing another context's domain or repository. `EnsureTypedControlAccount` is internal, system-authority-only, and deterministic per `(user, role, subject_reference, currency)`. Publish only the frozen `LedgerEventV1` DTOs through the Phase 1 outbox envelope.

- [ ] **Step 4 — Run focused persistence and contract tests**

Run:

```bash
cargo test --test ledger_public_contracts
cargo test --test ledger_persistence internal_command
```

Expected: PASS.

- [ ] **Step 5 — REFACTOR: enforce the dependency direction**

Run:

```bash
rg -n "crate::contexts::(banking|sharing|loans|portfolio)::" src/contexts/ledger
```

Expected: no matches. Later contexts import `ledger::public`; Ledger never imports them.

- [ ] **Step 6 — Commit boundary**

```bash
git add src/contexts/ledger/application/internal_commands.rs src/contexts/ledger/application/mod.rs src/contexts/ledger/public.rs tests/ledger_public_contracts.rs tests/ledger_persistence.rs
git commit -m "feat(ledger): freeze process-manager accounting contracts"
```

---

## Task 13: End-to-end audit trail and final verification

**Files:**
- Modify: `tests/ledger_api_v2.rs`
- Modify: `tests/ledger_persistence.rs`
- Modify: `tests/ledger_public_contracts.rs`
- Read: all Phase 2 files

- [ ] **Step 1 — Add the full money lifecycle test**

Through the isolated V2 router for user actions, plus the provider-neutral public facade for an external balance observation:

1. open cash with a visible opening balance;
2. open a debit card at zero;
3. create an expense;
4. transfer cash to card with a fee;
5. correct the card to a target balance with a reason;
6. reverse the expense;
7. replace it with a corrected amount;
8. edit its annotation;
9. archive the non-zero card, prove ordinary activity is blocked but history/balance remain visible, then restore it;
10. record a zero-delta observation and verify a visible matched case with no correction entry;
11. record a non-zero observation, approve it through `POST /reconciliations/{id}/approve`, and verify the correction;
12. create an intervening entry for a second case and verify stale-balance approval is rejected; and
13. list account activity and assert every effect, source, actor, relation, observation, and before/target/delta is visible.

- [ ] **Step 2 — RED: prove tampering is detected**

Inside a test-only transaction, corrupt one projection, verify the operational check reports it, rebuild, and re-run the lifecycle assertions. Also attempt direct journal/posting update/delete and expect database rejection.

- [ ] **Step 3 — GREEN: make only the minimum query/test support changes**

Do not weaken immutability triggers or expose rebuild over HTTP to make the test pass.

- [ ] **Step 4 — Run full verification**

Run:

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --test shared_kernel
cargo test --test context_boundaries
cargo test --test v2_migrations
cargo test --test ledger_domain
cargo test --test ledger_persistence
cargo test --test ledger_concurrency
cargo test --test ledger_public_contracts
cargo test --test ledger_api_v2
cargo test --test openapi_v2
```

Expected: PASS.

- [ ] **Step 5 — Run the forbidden-pattern audit**

Run:

```bash
rg -n "set_balance|adjust_balance|DELETE FROM ledger\.(journal_entries|postings)|UPDATE ledger\.(journal_entries|postings)|f32|f64" src/contexts/ledger src/infrastructure/migrations_v2/0003_ledger.sql
rg -n "monobank|subscription|provider" src/contexts/ledger
```

Expected: no prohibited mutation or provider coupling. The term `provider` may appear only in a neutral account-authority enum or comments/tests explaining the boundary; inspect every match.

- [ ] **Step 6 — Commit only if the lifecycle test changed files**

```bash
git add tests/ledger_api_v2.rs tests/ledger_persistence.rs tests/ledger_public_contracts.rs <minimum-support-files>
git commit -m "test(ledger): verify the complete visible money lifecycle"
```

---

## Verification commands

Task 13 is the canonical Phase 2 gate. Run:

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --test shared_kernel
cargo test --test context_boundaries
cargo test --test v2_migrations
cargo test --test ledger_domain
cargo test --test ledger_persistence
cargo test --test ledger_concurrency
cargo test --test ledger_public_contracts
cargo test --test ledger_api_v2
cargo test --test openapi_v2
```

Also run the forbidden-pattern audits in Task 13 and inspect every permitted neutral `provider` match.

## Commit boundaries

| Commit | Required outcome |
|--------|------------------|
| `feat(ledger): model account aggregate and system policy` | Account invariants exist without persistence |
| `feat(ledger): enforce immutable balanced journals` | Double entry and immutable relations exist in domain |
| `feat(db): add strict double-entry Ledger schema` | PostgreSQL independently rejects corrupt journals |
| `feat(ledger): add transactional aggregate unit of work` | Financial writes can commit/rollback as one unit |
| `feat(ledger): implement idempotent account commands` | Opening balance is a journal, not a seeded column |
| `feat(ledger): post manual income and expense journals` | Manual scalar intent becomes balanced postings |
| `feat(ledger): post atomic transfers with fees and FX` | Every leg commits together with deterministic locks |
| `feat(ledger): make financial corrections immutable and visible` | Corrections/reversals/replacements never erase history |
| `feat(ledger): add auditable queries and projection verification` | Balances are fast, inspectable, and rebuildable |
| `feat(api): build isolated Finance V2 Ledger endpoints` | Unversioned replacement contract is testable without runtime cutover |
| `feat(ledger): add version-fenced balance reconciliation` | Observations never overwrite balance; approval is explicit and race-safe |
| `feat(ledger): freeze process-manager accounting contracts` | Later contexts have typed, non-CRUD Ledger entry points |
| `test(ledger): verify the complete visible money lifecycle` | User-facing auditability is proven end to end |

## Exit criteria

- [ ] Every committed journal entry has at least two postings and balances to zero independently per currency in both Rust and PostgreSQL.
- [ ] Journal entries, postings, correction details, and audit events cannot be updated or deleted.
- [ ] Account balances are projections and equal the signed sum of postings after normal, replayed, failed, and concurrent commands.
- [ ] Opening balances and target-balance corrections are explicit journals visible in account activity.
- [ ] Transfers, fees, and FX legs share one UoW and lock accounts deterministically.
- [ ] Reversal and replacement preserve originals; annotations are separately versioned/audited.
- [ ] Category IDs are accepted only after same-user/active validation through `classification::public`; Ledger never queries Classification tables.
- [ ] Zero-delta observations remain visible; non-zero reconciliation approval is an explicit correction and rejects stale balance versions.
- [ ] Typed commands/results exist for external import/state/reversal, Sharing reclassification/settlement, loan accounting, and Portfolio cash/control-account settlement without reverse context imports.
- [ ] Versioned Ledger command/result/query/event DTOs have golden serialization tests and are frozen for downstream phases.
- [ ] Every financial POST is idempotent and payload-sensitive; stale aggregate mutation returns conflict.
- [ ] Every tenant-owned lookup/write is scoped by authenticated `user_id`; cross-user rows appear not found.
- [ ] The replacement API/OpenAPI use decimal strings, expose source/actor/timestamps/effects/relationships, and agree on every route/header/error schema.
- [ ] No hard-delete or direct-balance financial API exists.
- [ ] An explicit V2 test database migrates through `0003`; a legacy database is rejected by the parallel V2 helper.
- [ ] The isolated replacement router passes, while runtime/default test migrators/legacy routes/Docker/environment remain byte-for-byte untouched for Phase 8.
- [ ] Existing legacy migration checksums remain frozen for historical audit only.

## Explicitly out of scope

- Monobank connection, discovery, webhook, provider event inbox, sync, and observed balances (Phase 3).
- Mail, Recurring, Reporting, and Sharing runtime behavior.
- Loans workflows, Portfolio/ОВДП positions and valuation, subscription recurrence, and Reporting projections.
- Importing any old account, transaction, token, subscription, or projection row.
- Legacy-compatible payload translation or dual-write behavior for `/accounts` or `/transactions`; Phase 8 reuses the unversioned paths with this breaking replacement contract.
