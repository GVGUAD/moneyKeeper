# Finance V2 Phase 6 — Loans Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Keep checkbox state in this document. Loan workflows call Ledger only through durable process managers.

**Goal:** Add borrowed and lent LoanAgreement lifecycle with contractual principal/terms, disbursement, principal repayment, interest, fees, manual interest accrual, write-off, reversal, and closure while keeping every monetary effect in Ledger and every component separately auditable.

**Dependencies:** Phases 1–5 are integrated and migrations pass through `0009`. Ledger liability/receivable accounts, typed loan-accounting/reversal contracts, idempotency, event feed, and durable process-manager runtime are stable. Reporting provides its Loan consumer contract and projection storage.

**Architecture:** Loans owns LoanAgreement, immutable term revisions, LoanMovement intent/history, confirmed principal/accrued-interest/accrued-fee projections, and workflow state. Counterparty is a Loans-owned contractual value object; Loans does not depend on Sharing contacts. Ledger owns the linked LoanPayable liability or LoanReceivable asset account, cash movements, journal entries, corrections, and reversals. Opening and every monetary movement use a durable state machine: commit Loans intent/outbox, call Ledger outside the transaction with a derived idempotency key, then confirm Posted or Failed in a second Loans transaction.

**Tech Stack:** Rust 2024, Axum 0.8, SQLx/PostgreSQL 16, Tokio, rust_decimal, Chrono, Serde, UUID, Finance V2 shared kernel, outbox/inbox/process runtime, Testcontainers.

**Spec:** docs/superpowers/specs/2026-08-05-finance-ddd-v2-design.md

---

## Non-negotiable decisions

- Migration is src/infrastructure/migrations_v2/0010_loans.sql. Legacy migrations remain untouched.
- V2 starts blank; there is no legacy Loan account/detail/transaction backfill.
- Phase 6 extends only the parallel V2 bootstrap and static/openapi.v2.json. Default runtime/API/DATABASE_URL remain legacy until Phase 8.
- Future promoted routes are unversioned. Do not add /v2 prefixes.
- Borrowed maps to a Ledger LoanPayable liability; Lent maps to a LoanReceivable asset.
- Principal, interest, and fee remain separate in domain commands, storage, Ledger command, events, API, and Reporting.
- Contractual fees are explicit: on a Borrowed agreement they are a borrower cost/payable; on a Lent agreement they are lender income/receivable. Unrelated bank/payment-rail charges remain ordinary Ledger transfer expenses rather than being inferred as loan fees.
- Principal movements are balance-sheet transfers, not income/expense.
- Confirmed principal, accrued-interest, and accrued-fee balances change only after Ledger confirms Posted.
- Posted LoanMovement is immutable. Correct by reversal and optional replacement.
- Term edits append revisions and never mutate Ledger rows.
- Manual interest accrual is supported; automatic amortization schedules and repayment reminders are not.
- Closure requires every confirmed component balance to be zero and no pending accounting/reversal process.
- No Loans SQL names a Ledger table.
- Every idempotent Loans command stores `(user_id, command_scope, idempotency_key, canonical_request_hash, durable_result)` in the same Loans UoW as aggregate, audit, and outbox changes. Same key plus same hash returns the stored result; same key plus a different hash returns `409 Conflict` with no new effect.

## Posting semantics

Borrowed disbursement:

- debit cash asset;
- credit loan liability principal.

Borrowed repayment:

- debit loan liability for principal;
- debit accrued interest/fee payables for previously accrued components;
- debit interest/fee expense only for current, not-yet-accrued components;
- credit cash for total payment.

Lent disbursement:

- debit loan receivable principal;
- credit cash asset.

Lent repayment:

- debit cash for total receipt;
- credit loan receivable for principal;
- credit accrued interest/fee receivables for previously accrued components;
- credit interest/fee income only for current, not-yet-accrued components.

Manual accrual:

- borrowed: debit interest/fee expense, credit the corresponding hidden accrued payable;
- lent: debit the corresponding hidden accrued receivable, credit interest/fee income.

Write-off:

- borrowed principal or accrued components: debit the liability/payable being forgiven and credit a separately classified debt-forgiveness income/contra-expense account according to the typed component;
- lent principal or accrued components: debit a separately classified bad-debt expense and credit the principal/accrued receivable being written off.

The visible principal account and hidden per-agreement/currency accrued-interest and accrued-fee control accounts are provisioned through Ledger's typed account policy. Repayment requests state how much applies to principal, previously accrued interest/fees, and current-period interest/fees; the system never guesses the allocation and never recognizes an accrued component as income/expense twice.

## Entry gate

- [ ] Ledger LoanPayable/LoanReceivable provisioning contract is frozen.
- [ ] Ledger typed loan command covers disbursement, repayment, accrual, fee, and write-off components.
- [ ] Ledger loan reversal returns a durable result under idempotency.
- [ ] Ledger Reporting events distinguish principal, interest, fee, and write-off.
- [ ] Process-manager crash/retry tests pass.

Run:

~~~bash
cargo test --test ledger_public_contracts
cargo test --test integration_runtime
cargo test --test context_boundaries
~~~

Expected: PASS.

---

## File map

| File | Action | Responsibility |
|---|---|---|
| src/infrastructure/migrations_v2/0010_loans.sql | Create | Loans schema, scoped command receipts, term/movement facts, projections, accounting status/correlations |
| src/contexts/loans/mod.rs | Modify Phase 1 skeleton | Loans exports |
| src/contexts/loans/public.rs | Modify Phase 1 skeleton | Commands/queries/events |
| src/contexts/loans/domain/terms.rs | Create | Counterparty, interest terms, dates |
| src/contexts/loans/domain/loan_agreement.rs | Create | Agreement aggregate/lifecycle |
| src/contexts/loans/domain/loan_movement.rs | Create | Monetary intent/status/reversal aggregate |
| src/contexts/loans/domain/error.rs | Create | Stable Loans errors |
| src/contexts/loans/domain/mod.rs | Create | Domain exports |
| src/contexts/loans/application/commands.rs | Create | Open/disburse/repay/accrue/write-off/reverse/replace/close DTOs |
| src/contexts/loans/application/handlers.rs | Create | Loans UoW orchestration |
| src/contexts/loans/application/queries.rs | Create | Agreement/movement/outstanding reads |
| src/contexts/loans/application/ports.rs | Create | UoW, repositories, Ledger port, outbox |
| src/contexts/loans/application/mod.rs | Create | Application exports |
| src/contexts/loans/infrastructure/repository.rs | Create | Aggregate persistence |
| src/contexts/loans/infrastructure/unit_of_work.rs | Create | SQLx Loans UoW |
| src/contexts/loans/infrastructure/projections.rs | Create | Confirmed principal/interest/fee balance projection |
| src/contexts/loans/infrastructure/queries.rs | Create | Read-side SQL |
| src/contexts/loans/infrastructure/mod.rs | Create | Infrastructure exports |
| src/contexts/loans/api/dto.rs | Create | Decimal-string task DTOs |
| src/contexts/loans/api/handlers.rs | Create | Loan handlers |
| src/contexts/loans/api/routes.rs | Create | Isolated V2 Loans router |
| src/contexts/loans/api/mod.rs | Create | API exports |
| src/integration/process_managers/loan_opening.rs | Create | Ledger loan-account provisioning |
| src/integration/process_managers/loan_accounting.rs | Create | Movement accounting coordinator |
| src/integration/process_managers/loan_reversal.rs | Create | Movement reversal coordinator |
| src/integration/process_managers/loan_replacement.rs | Create | Durable reverse-then-post replacement saga |
| src/integration/process_managers/mod.rs | Modify | Register Loan managers |
| src/contexts/reporting/public.rs | Modify | Accept Loans event versions |
| src/contexts/reporting/infrastructure/loan_projection.rs | Create | Liability/receivable/interest projections |
| src/api/v2.rs | Modify | Compose Loans routes into the isolated replacement router |
| src/bootstrap/v2.rs | Modify | Construct Loans only in V2 bootstrap |
| src/contexts/mod.rs | Modify | Export Loans public surface |
| static/openapi.v2.json | Modify | Add future unversioned Loan routes |
| tests/loans_domain.rs | Create | Agreement/terms/movement tests |
| tests/loans_persistence.rs | Create | Constraints/UoW/concurrency tests |
| tests/loans_api.rs | Create | Parallel V2 API/idempotency/auth tests |
| tests/loans_accounting.rs | Create | Posting/crash/retry tests |
| tests/loans_reversal.rs | Create | Reversal/replacement/closure tests |
| tests/reporting_loans.rs | Create | Principal/interest/liability projection tests |
| tests/phase6_workflow.rs | Create | Borrowed/lent end-to-end workflow |

---

## Task 1: Create the Loans schema and database invariants

**Files:**

- Create: src/infrastructure/migrations_v2/0010_loans.sql
- Create: tests/loans_persistence.rs

- [ ] **Step 1 — RED: write fresh-database tests**

Cover:

- loans.agreements with unique id/user, direction, contractual principal/currency, counterparty, linked opaque Ledger account ID, lifecycle/version;
- append-only term revisions;
- immutable movement intent/components/status history;
- confirmed principal, accrued-interest, and accrued-fee projections constrained non-negative;
- opaque Ledger journal/reversal links plus user, never foreign-context FKs;
- agreement/movement accounting status and process correlation; generic attempts, leases, retry/error, and fencing remain in the Phase 1 `integration` process store;
- inbox/outbox plus `loans.command_receipts` unique on `(user_id, command_scope, idempotency_key)`, with canonical request hash and durable serialized result/status;
- composite tenant safety and state checks;
- update/delete rejection for Posted movement components and term history.

- [ ] **Step 2: run and capture RED**

~~~bash
SQLX_OFFLINE=true cargo test --test loans_persistence schema_ -- --nocapture
~~~

Expected: FAIL because migration 0010 does not exist.

- [ ] **Step 3 — GREEN: add migration**

Create schema loans with bounded NUMERIC, TIMESTAMPTZ, tenant-safe composite constraints, immutable-fact triggers, and indexes for lifecycle, movement sequence, retry, and activity reads.

- [ ] **Step 4: run focused tests**

~~~bash
SQLX_OFFLINE=true cargo test --test loans_persistence schema_ -- --nocapture
~~~

Expected: PASS.

- [ ] **Step 5 — REFACTOR: inspect ownership**

~~~bash
rg -n "REFERENCES (ledger|reporting|sharing)\\." src/infrastructure/migrations_v2/0010_loans.sql
~~~

Expected: no matches.

- [ ] **Step 6: commit**

~~~bash
git add src/infrastructure/migrations_v2/0010_loans.sql tests/loans_persistence.rs
git commit -m "feat(loans): add v2 loans schema"
~~~

---

## Task 2: Implement terms and LoanAgreement aggregate

**Files:**

- Create: src/contexts/loans/domain/{terms,loan_agreement,error,mod}.rs
- Modify: src/contexts/loans/{mod,public}.rs
- Create: tests/loans_domain.rs
- Modify: src/contexts/mod.rs

- [ ] **Step 1 — RED: write agreement tests**

Cover Borrowed/Lent mapping, non-empty counterparty, positive contractual principal, one currency, start/due date order, optional simple annual rate metadata, Draft/PendingAccounting/Active/Failed/Closed lifecycle, immutable direction/currency, append-only term revision, stale expected version, and close requiring zero principal/accrued-interest/accrued-fee balances with no pending movement.

- [ ] **Step 2: run RED**

~~~bash
cargo test --test loans_domain -- --nocapture
~~~

Expected: FAIL.

- [ ] **Step 3 — GREEN: implement aggregate**

Terms record contractual facts only. Ledger does not calculate schedules; Loans does not auto-accrue.

- [ ] **Step 4: run focused tests**

~~~bash
cargo test --test loans_domain -- --nocapture
~~~

Expected: PASS.

- [ ] **Step 5 — REFACTOR: explicit revision history**

Methods return domain events/errors; no public term-field mutation.

- [ ] **Step 6: commit**

~~~bash
git add src/contexts/loans src/contexts/mod.rs tests/loans_domain.rs
git commit -m "feat(loans): add agreement and terms aggregate"
~~~

---

## Task 3: Implement LoanMovement and accounting recipes

**Files:**

- Create: src/contexts/loans/domain/loan_movement.rs
- Modify: src/contexts/loans/domain/mod.rs
- Modify: tests/loans_domain.rs

- [ ] **Step 1 — RED: write movement tests**

Cover:

- origination/disbursement positive principal;
- repayment with separate principal/interest/fee;
- at least one positive repayment component;
- principal repayment/write-off cannot exceed confirmed principal outstanding;
- cumulative posted principal disbursements cannot exceed contractual principal unless an append-only term revision raises it;
- one agreement currency;
- borrowed/lent posting recipes;
- manual interest accrual;
- repayment allocation between previously accrued interest/fees and current-period interest/fees is explicit rather than inferred;
- write-off with mandatory reason;
- PendingAccounting/Posted/Failed states;
- deterministic Ledger idempotency key by movement ID;
- failed accounting has no principal effect;
- posted movement reverses once and remains visible;
- replacement links to reversed movement.

- [ ] **Step 2: run RED**

~~~bash
cargo test --test loans_domain -- --nocapture
~~~

Expected: FAIL.

- [ ] **Step 3 — GREEN: implement movement aggregate/pure recipes**

Domain decides components and the signed effect on each confirmed component balance. Application anti-corruption adapter translates that decision into Ledger public commands.

- [ ] **Step 4: run focused tests**

~~~bash
cargo test --test loans_domain -- --nocapture
~~~

Expected: PASS.

- [ ] **Step 5 — REFACTOR: isolate component effects**

One pure signed component-effect function is used for post/reversal projection updates. Interest and fee never change principal; applied-accrual and current-period amounts remain distinguishable.

- [ ] **Step 6: commit**

~~~bash
git add src/contexts/loans/domain tests/loans_domain.rs
git commit -m "feat(loans): model separated loan movements"
~~~

---

## Task 4: Persist Loans through one UoW

**Files:**

- Create: src/contexts/loans/application/{commands,handlers,queries,ports,mod}.rs
- Create: src/contexts/loans/infrastructure/{repository,unit_of_work,projections,queries,mod}.rs
- Modify: src/contexts/loans/public.rs
- Modify: tests/loans_persistence.rs

- [ ] **Step 1 — RED: write repository/UoW/concurrency tests**

Prove aggregate round trips, append-only revisions/movements, agreement plus projection/idempotency/audit/outbox atomicity, rollback on outbox failure, optimistic conflict, same key plus the same canonical request hash returning the original durable result, same key plus a different hash returning a typed conflict with no state/outbox change, cross-user rejection, and two concurrent principal reductions against one expected outstanding version allowing one winner. Lease reclaim remains a shared integration-runtime/process-manager test.

- [ ] **Step 2: run RED**

~~~bash
cargo test --test loans_persistence -- --nocapture
~~~

Expected: FAIL.

- [ ] **Step 3 — GREEN: implement repositories and Loans UoW**

One repository per aggregate; process store is separate from aggregate status; read SQL stays in query adapter. The Loans UoW inserts/locks `loans.command_receipts` and commits the request hash and durable result atomically with aggregate, projection, audit, and outbox changes.

- [ ] **Step 4: run focused tests**

~~~bash
cargo test --test loans_persistence -- --nocapture
~~~

Expected: PASS.

- [ ] **Step 5 — REFACTOR: keep Ledger opaque**

No repository/SQL imports Ledger domain/application/infrastructure or tables.

- [ ] **Step 6: commit**

~~~bash
git add src/contexts/loans tests/loans_persistence.rs
git commit -m "feat(loans): persist agreements and movements atomically"
~~~

---

## Task 5: Add parallel Loan API and durable opening

**Files:**

- Create: src/contexts/loans/api/{dto,handlers,routes,mod}.rs
- Create: src/integration/process_managers/loan_opening.rs
- Create: tests/loans_api.rs
- Modify: src/integration/process_managers/mod.rs
- Modify: src/contexts/loans/application/ports.rs
- Modify: src/contexts/loans/public.rs
- Modify: src/api/v2.rs
- Modify: src/bootstrap/v2.rs
- Modify: static/openapi.v2.json

- [ ] **Step 1 — RED: write API/opening tests**

Cover every required unversioned route below, `Idempotency-Key`, body `expected_version`, same-key/same-canonical-hash replay returning the stored status/body, same-key/different-hash returning `409 Conflict` without another outbox/process effect, decimal-string Money, tenant isolation, Borrowed provisioning LoanPayable, Lent provisioning LoanReceivable, 202 plus process state, Ledger key loan-open:{agreement_id}, crash after account creation, transient retry, visible terminal failure, and no duplicate account.

Required promoted routes (mounted at these exact paths only in Phase 8; none are optional or aliases):

~~~text
GET  /loans
GET  /loans/{id}
GET  /loans/{id}/term-revisions
GET  /loans/{id}/movements
GET  /loans/{id}/movements/{movement_id}
POST /loans
POST /loans/{id}/term-revisions
POST /loans/{id}/closure
POST /loans/{id}/disbursements
POST /loans/{id}/repayments
POST /loans/{id}/interest-accruals
POST /loans/{id}/write-offs
POST /loans/{id}/movements/{movement_id}/reversals
POST /loans/{id}/movements/{movement_id}/replacements
~~~

Every POST above requires `Idempotency-Key` and the canonical-hash replay/conflict contract. `POST /loans` creates version 1 and therefore has no `expected_version`; every other POST carries body `expected_version` for the current LoanAgreement and rejects missing/stale values before creating movement/process/outbox state. Movement reversal/replacement also validates the referenced immutable movement and its once-only relationship while fencing the agreement through `expected_version`.

- [ ] **Step 2: run RED**

~~~bash
cargo test --test loans_api -- --nocapture
~~~

Expected: FAIL.

- [ ] **Step 3 — GREEN: implement isolated V2 API and opening manager**

Sequence PendingAccounting to Ledger account result to Active/Failed, with every transition committed separately. Compose routes only in `src/api/v2.rs`; do not mount in the default runtime.

- [ ] **Step 4: run focused tests**

~~~bash
cargo test --test loans_api -- --nocapture
~~~

Expected: PASS.

- [ ] **Step 5 — REFACTOR: cross-context calls outside SQL**

Block Ledger fake and prove no Loans SQL transaction remains open.

- [ ] **Step 6: commit**

~~~bash
git add src/contexts/loans/api src/integration/process_managers/loan_opening.rs src/integration/process_managers/mod.rs src/api/v2.rs src/bootstrap/v2.rs static/openapi.v2.json tests/loans_api.rs
git commit -m "feat(loans): add durable loan opening API"
~~~

---

## Task 6: Implement durable movement accounting

**Files:**

- Create: src/integration/process_managers/loan_accounting.rs
- Create: tests/loans_accounting.rs
- Modify: src/integration/process_managers/mod.rs
- Modify: src/contexts/loans/application/{commands,handlers,ports}.rs
- Modify: src/contexts/loans/api/{dto,handlers,routes}.rs
- Modify: src/contexts/loans/public.rs
- Modify: static/openapi.v2.json

- [ ] **Step 1 — RED: write accounting workflow tests**

Cover borrowed/lent disbursement; borrowed repayment principal/interest/fee; lent receipt principal/interest/fee; partial repayment; manual accrual; explicit repayment against accrued versus current-period interest/fees; no double recognition of accrued income/expense; component overpayment rejection; contractual-principal cap; write-off by component and direction; 202 process state; key loan-accounting:{movement_id}; crash after Ledger commit; transient/terminal failure; component balances changing only after Posted; concurrent repayment protection; Ledger component result retained.

The disbursement, repayment, interest-accrual, and write-off handlers and OpenAPI entries use the exact paths declared in Task 5 and require the agreement `expected_version` plus `Idempotency-Key`.

- [ ] **Step 2: run RED**

~~~bash
cargo test --test loans_accounting -- --nocapture
~~~

Expected: FAIL.

- [ ] **Step 3 — GREEN: implement accounting process**

Persist movement intent/process/outbox, call typed Ledger command outside Loans transaction, then lock the component projection and confirm Posted/component effects/outbox atomically.

- [ ] **Step 4: run focused tests**

~~~bash
cargo test --test loans_accounting -- --nocapture
~~~

Expected: PASS.

- [ ] **Step 5 — REFACTOR: one coordinator, typed recipes**

Reuse process mechanics across movement types without collapsing their domain validation or components.

- [ ] **Step 6: commit**

~~~bash
git add src/integration/process_managers/loan_accounting.rs src/integration/process_managers/mod.rs src/contexts/loans static/openapi.v2.json tests/loans_accounting.rs
git commit -m "feat(loans): durably post loan accounting"
~~~

---

## Task 7: Implement reversal, replacement, write-off, and closure

**Files:**

- Create: src/integration/process_managers/loan_reversal.rs
- Create: src/integration/process_managers/loan_replacement.rs
- Create: tests/loans_reversal.rs
- Modify: src/integration/process_managers/mod.rs
- Modify: src/contexts/loans/application/{commands,handlers,ports}.rs
- Modify: src/contexts/loans/api/{dto,handlers,routes}.rs
- Modify: src/contexts/loans/public.rs
- Modify: static/openapi.v2.json

- [ ] **Step 1 — RED: write reversal/closure tests**

Cover the exact closure/reversal/replacement routes frozen in Task 5; required `Idempotency-Key`; missing/stale LoanAgreement `expected_version`; same-hash durable replay; different-hash `409`; Posted-only reversal; required reason; once-only process; Ledger reversal before inverse component effects; crash/retry; repayment reversal restoring the exact principal/accrual components; interest/fee never changing principal; replacement correlation; crash after the original Ledger reversal and after its Loans confirmation but before replacement posting; component write-off classification; close blocked by any nonzero component balance/pending process; and all old facts visible.

The handlers/OpenAPI use `POST /loans/{id}/write-offs`, `POST /loans/{id}/closure`, `POST /loans/{id}/movements/{movement_id}/reversals`, and `POST /loans/{id}/movements/{movement_id}/replacements` exactly as declared in Task 5.

- [ ] **Step 2: run RED**

~~~bash
cargo test --test loans_reversal -- --nocapture
~~~

Expected: FAIL.

- [ ] **Step 3 — GREEN: implement reversal coordinator and close command**

Use `loan-reversal:{movement_id}`; confirm returned Ledger reversal ID and inverse component effects in one Loans UoW.

Replacement is an explicit durable saga with immutable replacement movement/payload and states `ReplacementRequested → ReversingOriginal → OriginalReversed → PostingReplacement → Posted`, plus visible `RetryDue` and `TerminalFailure` outcomes. Use separate stable Ledger keys `loan-replacement:{original_movement_id}:{replacement_movement_id}:reverse` and `loan-replacement:{original_movement_id}:{replacement_movement_id}:post`. First idempotently reverse the original and confirm its inverse component effects/status in Loans; only then post the replacement and confirm its new component effects. A crash at either cross-context boundary resumes from the stored state/receipts. If posting the replacement is terminally rejected after reversal, expose `ReplacementFailedAfterReversal`, keep projections consistent with the reversed original, block closure, and allow an explicit retry/new replacement workflow—never pretend the original is still active or hide an indefinite half-state.

- [ ] **Step 4: run focused tests**

~~~bash
cargo test --test loans_reversal -- --nocapture
~~~

Expected: PASS.

- [ ] **Step 5 — REFACTOR: shared confirmation math**

Posting and reversal use one tested component projection function with explicit sign/correlation.

- [ ] **Step 6: commit**

~~~bash
git add src/integration/process_managers/loan_reversal.rs src/integration/process_managers/loan_replacement.rs src/integration/process_managers/mod.rs src/contexts/loans static/openapi.v2.json tests/loans_reversal.rs
git commit -m "feat(loans): reverse correct and close loans"
~~~

---

## Task 8: Project Loans into Reporting and reconcile with Ledger

**Files:**

- Create: src/contexts/reporting/infrastructure/loan_projection.rs
- Create: tests/reporting_loans.rs
- Modify: src/contexts/reporting/public.rs
- Modify: src/contexts/reporting/application/projectors.rs
- Modify: src/contexts/reporting/infrastructure/mod.rs

- [ ] **Step 1 — RED: write Reporting tests**

Cover borrowed outstanding as liability, lent outstanding as receivable asset, principal excluded from income/expense, interest/fees correctly classified, manual accrual, write-off, reversal, no double count with linked Ledger account, duplicate events, exact replay, and mismatch alert between confirmed Loans outstanding and Ledger public export without automatic repair.

- [ ] **Step 2: run RED**

~~~bash
cargo test --test reporting_loans
~~~

Expected: FAIL.

- [ ] **Step 3 — GREEN: implement Loan event consumer**

Consume versioned public events only. Reconciliation produces an operational alert; it never adjusts Loans or Ledger.

- [ ] **Step 4: run focused tests**

~~~bash
cargo test --test reporting_loans
~~~

Expected: PASS.

- [ ] **Step 5 — REFACTOR: report no-double-count invariant**

Assert cash, liability/receivable, interest, and net-worth compose exactly once.

- [ ] **Step 6: commit**

~~~bash
git add src/contexts/reporting tests/reporting_loans.rs
git commit -m "feat(reporting): project loan balances and income"
~~~

---

## Task 9: Wire V2-only Loans and prove borrowed/lent crash workflows

**Files:**

- Create: tests/phase6_workflow.rs
- Modify: src/bootstrap/v2.rs
- Modify: src/integration/process_managers/mod.rs
- Modify: static/openapi.v2.json

- [ ] **Step 1 — RED: write full workflows**

Borrowed scenario: open, crash/recover account provisioning, disburse, manually accrue, repay principal/interest/fee, race repayments, reverse after Ledger commit crash, replace, write off remainder, close, rebuild Reporting.

Lent scenario: open receivable, disburse, partial principal/interest receipt, reverse, final receipt, close, and assert cash/receivable/income.

- [ ] **Step 2: run RED**

~~~bash
cargo test --test phase6_workflow -- --nocapture
~~~

Expected: FAIL.

- [ ] **Step 3 — GREEN: wire Loans in V2 bootstrap**

Register isolated router/event consumers/bounded process managers. Do not touch default main/router/DATABASE_URL.

- [ ] **Step 4: run Phase 6 suite**

~~~bash
cargo test --test loans_domain
cargo test --test loans_persistence
cargo test --test loans_api
cargo test --test loans_accounting
cargo test --test loans_reversal
cargo test --test reporting_loans
cargo test --test phase6_workflow
~~~

Expected: PASS.

- [ ] **Step 5 — REFACTOR: architecture scan**

~~~bash
cargo test --test context_boundaries
rg -n "(FROM|JOIN|UPDATE|INTO) (ledger|reporting|sharing)\\." src/contexts/loans
~~~

Expected: no foreign-context SQL.

- [ ] **Step 6: commit**

~~~bash
git add src/bootstrap/v2.rs src/integration/process_managers/mod.rs static/openapi.v2.json tests/phase6_workflow.rs
git commit -m "feat(phase6): complete borrowed and lent loans"
~~~

---

## Verification commands

~~~bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
SQLX_OFFLINE=true cargo test --all-targets -- --nocapture
cargo test --test openapi_v2
jq empty static/openapi.v2.json
git diff --exit-code -- src/infrastructure/migrations tests/migrations.rs
cargo test --test migrations
~~~

## Commit boundaries

1. Loans schema/invariants.
2. Agreement/terms aggregate.
3. Separated movement components.
4. Loans UoW/persistence.
5. Durable agreement opening/account provisioning API.
6. Disbursement/repayment/accrual/write-off accounting process.
7. Reversal/replacement/closure.
8. Reporting loan projections.
9. Integrated borrowed/lent workflows.

Keep contract terms, principal arithmetic, Ledger coordination, and Reporting projection changes in separate reviewable commits.

## Exit criteria

- [ ] Borrowed/Lent map to Ledger liability/receivable accounts.
- [ ] Contractual principal/currency/terms/lifecycle remain Loans-owned.
- [ ] Disbursement, repayment, interest, fees, accrual, write-off, reversal, closure are supported.
- [ ] Principal/interest/fee stay separate through domain, API, Ledger, events, Reporting.
- [ ] Every movement exposes PendingAccounting, Posted, or Failed and is retry-safe.
- [ ] Loans command receipts enforce same-key/same-hash durable replay and same-key/different-hash conflict with no duplicate effect.
- [ ] Outstanding changes only after confirmed Ledger accounting.
- [ ] Posted movements are reversed/replaced, never edited/deleted.
- [ ] The mandatory GET, term-revision, closure, movement-reversal, and movement-replacement paths are exact; every existing-agreement POST rejects missing/stale `expected_version` and every POST enforces `Idempotency-Key`.
- [ ] No amortization scheduler or reminder engine exists.
- [ ] Reporting excludes principal from income/expense and avoids double count.
- [ ] No Loans SQL names a foreign context table.
- [ ] Parallel V2 OpenAPI validates; default runtime/API/DATABASE_URL remain legacy.
- [ ] Blank migration, domain, DB, API, process, Reporting, format, clippy, boundary tests pass.

## Out of scope

- Automatic amortization schedules or repayment reminders.
- Variable/index-linked or compound interest engines.
- Multi-currency loans, collateral, credit bureau, payment initiation.
- Sharing Contact invitations/links.
- Legacy loan backfill.
- Mounting V2 routes or switching DATABASE_URL before Phase 8.
