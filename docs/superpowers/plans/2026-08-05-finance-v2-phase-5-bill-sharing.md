# Finance V2 Phase 5 — Contacts-First Split the Bill Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Keep checkbox state in this document. Build Contact before BillSplit and preserve multiple payers as a first-class invariant.

**Goal:** Add user-owned external contacts and auditable shared bills with multiple payers, exact/equal participant shares, deterministic obligations, existing-payment allocation, manual payment, partial settlement, and visible reversal.

**Dependencies:** Phases 1–4 are integrated and migrations pass through `0008`. Ledger exposes frozen typed contracts for contact receivable/payable control accounts, reclassification of existing imported/outgoing journals, manual Sharing payment, settlement, reversal, and user-scoped journal summaries. The durable process-manager runtime is stable, and Reporting provides its public Sharing consumer contract and bill-position projection storage.

**Architecture:** Sharing is one bounded context containing Contact, BillSplit, BillRevision, Contribution, ParticipantShare, Obligation, and Settlement. Contact is a user-owned external person and never an application identity. Contribution and share totals independently equal the bill total in minor units. A deterministic waterfall derives debtor-to-creditor obligations. Current-user contributions may allocate several existing outgoing Ledger journals or request one typed manual payment; contact contributions are Sharing facts. Durable accounting and settlement process managers call Ledger public commands outside Sharing transactions and expose PendingAccounting, Active/Posted, Failed, and retry state.

**Tech Stack:** Rust 2024, Axum 0.8, SQLx/PostgreSQL 16, Tokio, rust_decimal, Chrono, Serde, UUID, Finance V2 shared kernel, outbox/inbox/process manager, Testcontainers.

**Spec:** docs/superpowers/specs/2026-08-05-finance-ddd-v2-design.md

---

## Non-negotiable decisions

- Migration is src/infrastructure/migrations_v2/0009_sharing.sql. Legacy migrations remain untouched.
- V2 starts blank; there is no legacy contact/bill/transaction backfill.
- Phase 5 extends only the parallel V2 bootstrap and static/openapi.v2.json. Default runtime/API/DATABASE_URL stay legacy until Phase 8.
- Future promoted routes are unversioned. Do not add /v2 prefixes.
- Contact is external, user-owned, and has no linked application-user field or invitation workflow.
- A participant is CurrentUser or Contact(ContactId); identity is never an embedded display name.
- Multiple Contributions are first-class. There is no single paid_by column.
- Phase 5 supports exact and equal participant shares only.
- Resolved participant shares are persisted as truth; they are never recalculated with a newer algorithm.
- Equal-split remainder goes to current user when included, then stable participant-ID order.
- Bill accounting uses hidden per-contact/currency Ledger receivable/payable accounts only for obligations involving current user.
- Contact-to-contact obligations never create Ledger effects.
- A bill cannot be revised/cancelled while active settlements exist; settlements must reverse first.
- Bill revision/cancellation is also blocked while bill accounting is `PendingAccounting`/retrying; the client receives the current process reference and must wait for a terminal result. A terminal failure proven to have no Ledger effect may be cancelled without a reversal, while crash-after-Ledger uncertainty must first be resolved through Ledger's idempotent receipt. This prevents late accounting after cancellation.
- Existing imported journals are reclassified by a new typed Ledger journal, never edited.
- Cross-context calls occur outside SQL transactions and use derived idempotency keys.
- Every idempotent Sharing command stores `(user_id, command_scope, idempotency_key, canonical_request_hash, durable_result)` in the same Sharing UoW as aggregate, audit, and outbox changes. Same key plus same hash returns the stored result; same key plus a different hash returns `409 Conflict` with no new effect.

## Ledger accounting recipes

For the current user, let `C` be their total contribution, `S` their participant share, `R` receivables created by obligations owed to them, and `P` payables created by obligations they owe. The persisted obligation engine must satisfy `R - P = C - S`, so the typed Ledger recipe balances as `S + R = C + P`.

- New/unlinked current-user bill accounting posts debit current-user Expense `S`, debit contact Receivable `R`, credit selected Cash/Card `C`, and credit contact Payable `P`. A selected payment account is required only when `C > 0`. Zero legs are omitted: `C = 0, S > 0` still records Expense/Payable; `S = 0, C > 0` records Receivable/Cash; and `C = S = 0` creates no Ledger command for that user. Every non-empty result still obeys Ledger's two-posting and per-currency rules.
- Existing outgoing journals already recognize the allocated contribution as expense. If `R > 0`, a new reclassification debits contact receivables and credits expense by `R`. If `P > 0`, it debits expense and credits contact payables by `P`. The original journals remain immutable, and the resulting net expense is exactly `S`.
- A contact paying the current user posts debit Cash and credit that contact's Receivable. The current user paying a contact posts debit that contact's Payable and credit Cash. Neither settlement is income or expense.
- Linking an imported incoming settlement reverses its provisional income classification into a Receivable credit; linking an imported outgoing settlement reverses its provisional expense classification into a Payable debit. These are new typed reclassification journals, not edits.
- Contact-to-contact contributions, obligations, and settlements create no Ledger postings for the current user.

Tests use concrete journals to prove these equations for `C > S`, `C < S`, `C = S`, `C = 0`, `S = 0`, a non-participating current user, multiple payers, multiple current-user payment journals, and partial/reversed settlements. Sharing sends typed facts only; it cannot submit these postings directly.

## Entry gate

- [ ] Ledger contact-control-account provisioning contract is frozen.
- [ ] Ledger Sharing payment/reclassification/settlement/reversal contracts are frozen.
- [ ] Ledger query façade returns tenant-safe journal summaries and allocatable amounts.
- [ ] Process manager exposes durable pending/posted/failed state and crash-safe retries.
- [ ] Reporting accepts versioned Sharing events without table joins.

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
| src/infrastructure/migrations_v2/0009_sharing.sql | Create | Sharing schema, immutable revisions/allocations, obligations, settlements, accounting status/correlations |
| src/contexts/sharing/mod.rs | Modify Phase 1 skeleton | Sharing exports |
| src/contexts/sharing/public.rs | Modify Phase 1 skeleton | Public commands/queries/events |
| src/contexts/sharing/domain/contact.rs | Create | Contact aggregate |
| src/contexts/sharing/domain/participant.rs | Create | CurrentUser/Contact participant identity |
| src/contexts/sharing/domain/allocation.rs | Create | Multiple contributions and exact/equal shares |
| src/contexts/sharing/domain/bill_split.rs | Create | BillSplit/BillRevision aggregate |
| src/contexts/sharing/domain/obligation.rs | Create | Deterministic net/waterfall algorithm |
| src/contexts/sharing/domain/settlement.rs | Create | Partial/full settlement aggregate |
| src/contexts/sharing/domain/error.rs | Create | Stable Sharing errors |
| src/contexts/sharing/domain/mod.rs | Create | Domain exports |
| src/contexts/sharing/application/commands.rs | Create | Contact/bill/settlement task commands |
| src/contexts/sharing/application/handlers.rs | Create | Sharing UoW orchestration |
| src/contexts/sharing/application/queries.rs | Create | Contact/bill/obligation/settlement reads |
| src/contexts/sharing/application/ports.rs | Create | Aggregate UoW, Ledger ports, outbox |
| src/contexts/sharing/application/mod.rs | Create | Application exports |
| src/contexts/sharing/infrastructure/repository.rs | Create | Aggregate persistence |
| src/contexts/sharing/infrastructure/unit_of_work.rs | Create | SQLx Sharing UoW |
| src/contexts/sharing/infrastructure/projections.rs | Create | Obligation/contact-balance projection |
| src/contexts/sharing/infrastructure/queries.rs | Create | Read-side SQL |
| src/contexts/sharing/infrastructure/mod.rs | Create | Infrastructure exports |
| src/contexts/sharing/api/dto.rs | Create | Decimal-string Sharing DTOs |
| src/contexts/sharing/api/handlers.rs | Create | Contact/bill/settlement handlers |
| src/contexts/sharing/api/routes.rs | Create | Isolated V2 Sharing router |
| src/contexts/sharing/api/mod.rs | Create | API exports |
| src/integration/process_managers/sharing_accounting.rs | Create | Bill accounting/reclassification coordinator |
| src/integration/process_managers/sharing_settlement.rs | Create | Settlement/reversal coordinator |
| src/integration/process_managers/mod.rs | Modify | Register Sharing managers |
| src/contexts/reporting/public.rs | Modify | Accept Sharing event versions |
| src/contexts/reporting/infrastructure/sharing_projection.rs | Create | Bill/contact position projector |
| src/api/v2.rs | Modify | Compose Sharing routes into the isolated replacement router |
| src/bootstrap/v2.rs | Modify | Construct Sharing only in V2 bootstrap |
| src/contexts/mod.rs | Modify | Export Sharing public surface |
| static/openapi.v2.json | Modify | Add future unversioned Sharing routes |
| tests/sharing_contacts.rs | Create | Contact domain/persistence/API tests |
| tests/sharing_allocations.rs | Create | Exact/equal/multiple-payer arithmetic tests |
| tests/sharing_persistence.rs | Create | DB constraints/immutability/UoW tests |
| tests/sharing_api.rs | Create | Parallel V2 API/idempotency/auth tests |
| tests/sharing_accounting.rs | Create | Bill process-manager crash/retry tests |
| tests/sharing_settlements.rs | Create | Partial/linked/manual/reversal tests |
| tests/reporting_sharing.rs | Create | No-double-count bill position tests |
| tests/phase5_workflow.rs | Create | End-to-end multiple-payer workflow |

---

## Task 1: Create the Sharing schema and database invariants

**Files:**

- Create: src/infrastructure/migrations_v2/0009_sharing.sql
- Create: tests/sharing_persistence.rs

- [ ] **Step 1 — RED: write fresh-database schema tests**

Cover:

- sharing.contacts with unique id/user, archive/version, no application-user link;
- sharing.bills and append-only bill_revisions;
- contribution allocations, participant shares, derived obligations, and current revision;
- Ledger references stored as opaque ID plus user, never cross-schema foreign keys;
- settlement and settlement allocation rows;
- aggregate accounting/settlement status and correlation references; generic attempts, leases, retries, and errors remain in the Phase 1 `integration` process store;
- context inbox/outbox plus `sharing.command_receipts` unique on `(user_id, command_scope, idempotency_key)`, with canonical request hash and durable serialized result/status;
- composite tenant safety on every child relation;
- one active revision, immutable active allocation facts, and valid statuses;
- database rejection of update/delete on posted settlements and active revision facts.

- [ ] **Step 2: run and capture RED**

~~~bash
SQLX_OFFLINE=true cargo test --test sharing_persistence schema_ -- --nocapture
~~~

Expected: FAIL because migration 0009 does not exist.

- [ ] **Step 3 — GREEN: add migration**

Create schema sharing using bounded NUMERIC, TIMESTAMPTZ, tenant-safe composite constraints, immutable-fact triggers, and deterministic ordering indexes. Aggregate checks that require sums across rows stay in locked application/UoW logic plus deferred database validation where practical.

- [ ] **Step 4: run focused tests**

~~~bash
SQLX_OFFLINE=true cargo test --test sharing_persistence schema_ -- --nocapture
~~~

Expected: PASS.

- [ ] **Step 5 — REFACTOR: inspect foreign ownership**

~~~bash
rg -n "REFERENCES (ledger|reporting|banking)\\." src/infrastructure/migrations_v2/0009_sharing.sql
~~~

Expected: no matches.

- [ ] **Step 6: commit**

~~~bash
git add src/infrastructure/migrations_v2/0009_sharing.sql tests/sharing_persistence.rs
git commit -m "feat(sharing): add v2 sharing schema"
~~~

---

## Task 2: Implement the Contact aggregate first

**Files:**

- Create: src/contexts/sharing/domain/{contact,error,mod}.rs
- Modify: src/contexts/sharing/{mod,public}.rs
- Create: tests/sharing_contacts.rs
- Modify: src/contexts/mod.rs

- [ ] **Step 1 — RED: write Contact tests**

Prove normalized non-empty display name, optional note, immutable owner, archive/restore, stale version conflict, historical bill visibility after archive, and absence of linked-user/invitation semantics.

- [ ] **Step 2: run RED**

~~~bash
cargo test --test sharing_contacts domain_
~~~

Expected: FAIL.

- [ ] **Step 3 — GREEN: implement Contact**

Use `ContactId` and `UserId`. Archiving prevents selection for a new bill but never rewrites or hides prior bill facts.

- [ ] **Step 4: run focused tests**

~~~bash
cargo test --test sharing_contacts domain_
~~~

Expected: PASS.

- [ ] **Step 5 — REFACTOR: isolate ContactName validation**

Name normalization is a value-object rule, not a persistence or HTTP concern.

- [ ] **Step 6: commit**

~~~bash
git add src/contexts/sharing src/contexts/mod.rs tests/sharing_contacts.rs
git commit -m "feat(sharing): add external Contact aggregate"
~~~

---

## Task 3: Implement multiple contributions and exact/equal shares

**Files:**

- Create: src/contexts/sharing/domain/{participant,allocation,obligation}.rs
- Modify: src/contexts/sharing/domain/mod.rs
- Create: tests/sharing_allocations.rs

- [ ] **Step 1 — RED: write arithmetic/property tests**

Cover:

- CurrentUser and Contact participants;
- many contributions from current user and contacts;
- participant universe is the union of payers and share recipients, allowing pay-only and share-only participants;
- current-user contribution allocated across several outgoing journal references;
- exact shares;
- equal shares at minor-unit boundaries;
- positive contribution rows, non-negative exact/resolved shares, and legitimate zero-minor-unit equal shares when total units are fewer than selected recipients;
- remainder to current user, then stable participant-ID order;
- duplicate participant identities, zero/negative contributions, negative shares, and an all-zero share set rejected;
- contribution sum and share sum each equal bill total;
- net equals contribution minus share with an absent side treated as zero;
- deterministic creditor/debtor waterfall, current user first then stable ID;
- obligations conserve every net, sum to zero, and never cross currencies.

- [ ] **Step 2: run RED**

~~~bash
cargo test --test sharing_allocations -- --nocapture
~~~

Expected: FAIL.

- [ ] **Step 3 — GREEN: implement pure allocation and obligation engines**

Convert Money to integer minor units for equal splitting, then persist resolved Money shares. Do not store only a rule that would be recalculated later.

- [ ] **Step 4: run focused tests**

~~~bash
cargo test --test sharing_allocations -- --nocapture
~~~

Expected: PASS.

- [ ] **Step 5 — REFACTOR: prove deterministic ordering**

Ensure no HashMap iteration order affects shares or obligations. Keep I/O out of domain arithmetic.

- [ ] **Step 6: commit**

~~~bash
git add src/contexts/sharing/domain tests/sharing_allocations.rs
git commit -m "feat(sharing): add deterministic multiple-payer allocation"
~~~

---

## Task 4: Implement BillSplit, revisions, and settlement rules

**Files:**

- Create: src/contexts/sharing/domain/{bill_split,settlement}.rs
- Modify: src/contexts/sharing/domain/mod.rs
- Modify: tests/sharing_allocations.rs

- [ ] **Step 1 — RED: write aggregate tests**

Cover:

- draft requires title, occurred_at, total/currency, contributions, and shares;
- bill currency is immutable across every revision and settlement;
- independent exact-total validation;
- PendingAccounting to Active/Failed;
- active allocation/revision immutability;
- revision retains prior facts and enters PendingAccounting;
- active settlements block revision/cancel;
- partial settlement reduces one obligation under aggregate lock;
- over-settlement rejected;
- manual or existing-journal settlement evidence;
- settlement reversal required before revision;
- bill/settlement reversals are visible once-only facts.
- settlement creation rejects missing/stale BillSplit `expected_version`, and settlement reversal rejects missing/stale Settlement `expected_version`.

- [ ] **Step 2: run RED**

~~~bash
cargo test --test sharing_allocations -- --nocapture
~~~

Expected: FAIL.

- [ ] **Step 3 — GREEN: implement aggregates**

Separate worker lease state from aggregate accounting status. Persist a revision's resolved contributions/shares/obligations as immutable truth.

- [ ] **Step 4: run focused tests**

~~~bash
cargo test --test sharing_allocations -- --nocapture
~~~

Expected: PASS.

- [ ] **Step 5 — REFACTOR: centralize locks/version policy**

Bill revision, settlement creation, and cancellation commands carry the current BillSplit `expected_version`; settlement reversal carries the current Settlement `expected_version`. The UoW later locks the bill and affected settlement/obligation rows in stable order.

- [ ] **Step 6: commit**

~~~bash
git add src/contexts/sharing/domain tests/sharing_allocations.rs
git commit -m "feat(sharing): model bill revisions and settlements"
~~~

---

## Task 5: Persist Sharing aggregates through one UoW

**Files:**

- Create: src/contexts/sharing/application/{commands,handlers,queries,ports,mod}.rs
- Create: src/contexts/sharing/infrastructure/{repository,unit_of_work,projections,queries,mod}.rs
- Modify: src/contexts/sharing/public.rs
- Modify: tests/sharing_contacts.rs
- Modify: tests/sharing_persistence.rs

- [ ] **Step 1 — RED: write repository/UoW/concurrency tests**

Prove Contact/BillSplit/Settlement round trips, append-only revisions, aggregate plus projection/idempotency/audit/outbox atomicity, rollback on injected outbox failure, optimistic conflict, same key plus the same canonical request hash returning the original durable result, same key plus a different hash returning a typed conflict with no state/outbox change, simultaneous settlement over-consumption allowing one winner, and cross-user rejection. Lease reclaim is covered through the shared integration runtime in the accounting workflow tests.

- [ ] **Step 2: run RED**

~~~bash
cargo test --test sharing_persistence -- --nocapture
~~~

Expected: FAIL.

- [ ] **Step 3 — GREEN: implement aggregate repositories and Sharing UoW**

One repository per aggregate; no table-shaped status repository. Contact-balance/obligation queries use read stores. The Sharing UoW inserts/locks `sharing.command_receipts` and commits the request hash and durable result atomically with aggregate, projection, audit, and outbox changes.

- [ ] **Step 4: run focused tests**

~~~bash
cargo test --test sharing_persistence -- --nocapture
~~~

Expected: PASS.

- [ ] **Step 5 — REFACTOR: keep cross-context IDs opaque**

Repository code must not import Ledger domain/application/infrastructure types or SQL.

- [ ] **Step 6: commit**

~~~bash
git add src/contexts/sharing tests/sharing_contacts.rs tests/sharing_persistence.rs
git commit -m "feat(sharing): persist aggregates atomically"
~~~

---

## Task 6: Add Contact and BillSplit parallel API

**Files:**

- Create: src/contexts/sharing/api/{dto,handlers,routes,mod}.rs
- Create: tests/sharing_api.rs
- Modify: src/api/v2.rs
- Modify: src/bootstrap/v2.rs
- Modify: static/openapi.v2.json

- [ ] **Step 1 — RED: write API tests**

Cover future unversioned Contact create/list/edit/archive and Bill create/get/list/revise/cancel routes; `Idempotency-Key`; `expected_version`; same-key/same-canonical-hash replay returning the stored status/body; same-key/different-hash returning `409 Conflict` without another outbox/process effect; many contributions; exact/equal share request and resolved response; several Ledger references on one current-user contribution; tenant isolation; decimal-string Money; and `202 Accepted` plus process state for accounting.

Required promoted routes (composed only in the isolated V2 router until Phase 8, then mounted at these exact unversioned paths):

~~~text
POST /contacts
GET  /contacts
GET  /contacts/{id}
PATCH /contacts/{id}
POST /contacts/{id}/archive
POST /bill-splits
GET  /bill-splits
GET  /bill-splits/{id}
POST /bill-splits/{id}/revisions
POST /bill-splits/{id}/settlements
POST /bill-splits/{id}/settlements/{settlement_id}/reversal
POST /bill-splits/{id}/cancellations
~~~

The cancellation route creates a visible cancellation/accounting-reversal process rather than deleting the bill. Contact metadata edits/archive require the Contact version. Bill revisions, settlement creation, and cancellation require body `expected_version` for BillSplit; settlement reversal requires body `expected_version` for Settlement. Tests cover missing/stale versions for both settlement routes. Every POST/PATCH command above requires `Idempotency-Key` and the canonical-hash replay/conflict contract; reads are side-effect free.

- [ ] **Step 2: run RED**

~~~bash
cargo test --test sharing_api
~~~

Expected: FAIL.

- [ ] **Step 3 — GREEN: implement isolated V2 router/OpenAPI**

Compose the routes only in `src/api/v2.rs`; do not mount in the default runtime. Commands save aggregate intent plus process/outbox and return observable PendingAccounting.

- [ ] **Step 4: run focused tests**

~~~bash
cargo test --test sharing_api
~~~

Expected: PASS.

- [ ] **Step 5 — REFACTOR: edge translation only**

HTTP DTOs do not enter domain or persistence ports.

- [ ] **Step 6: commit**

~~~bash
git add src/contexts/sharing/api src/api/v2.rs src/bootstrap/v2.rs static/openapi.v2.json tests/sharing_api.rs
git commit -m "feat(api): add parallel contacts-first sharing API"
~~~

---

## Task 7: Implement durable bill accounting and reclassification

**Files:**

- Create: src/integration/process_managers/sharing_accounting.rs
- Create: tests/sharing_accounting.rs
- Modify: src/integration/process_managers/mod.rs
- Modify: src/contexts/sharing/application/ports.rs
- Modify: src/contexts/sharing/public.rs

- [ ] **Step 1 — RED: write process-manager tests**

Cover:

- validate contacts and all current-user Ledger allocation references;
- several referenced outgoing journals allocated without over-allocation;
- manual current-user payment from selected account;
- create hidden contact receivable/payable control accounts per contact/currency;
- prove the `S + R = C + P` recipe and net Expense `S` for current-user overpayment, underpayment, and exact payment;
- no Ledger effect for contact-to-contact obligation;
- linked imported payment is reclassified by a new typed journal;
- revision accounting reverses the prior revision's Ledger effect before posting the replacement, while both correlations remain visible;
- cancellation with no active settlements durably reverses the active revision's accounting exactly once and reaches Cancelled only after confirmation;
- cancellation of a terminal failed revision with a proven no-financial-effect result emits a distinct no-effect cancellation outcome and never fabricates a reversal journal;
- cancellation is rejected while an active settlement exists and retains a visible Failed/PendingCancellation outcome on accounting failure;
- revision/cancellation is rejected while the current accounting process is pending/retrying; crash-after-Ledger recovery resolves the prior command receipt before either becomes eligible;
- confirmed cancellation emits `sharing.bill-cancelled.v1` exactly once with bill/revision/version, correlation, cancellation reason/time, and the final zero active-obligation position; retry returns the stored command result and does not append a duplicate event;
- process state PendingAccounting/Posted/Failed with errors;
- crash/retry between prior-revision reversal and replacement posting;
- crash after Ledger commit returns existing idempotent result;
- transient retry/backoff and visible terminal validation failure;
- bill becomes Active only after all required accounting succeeds.

- [ ] **Step 2: run RED**

~~~bash
cargo test --test sharing_accounting
~~~

Expected: FAIL.

- [ ] **Step 3 — GREEN: implement durable coordinator**

Use derived keys `sharing-bill-accounting:{bill_id}:{revision}` and `sharing-bill-accounting-reversal:{bill_id}:{prior_revision}`. For a revision, durably reverse the prior revision's accounting before posting the replacement. Never hold a Sharing transaction while calling Ledger. Persist every transition, returned journal/reversal ID, no-financial-effect result, and correlation. Confirm cancellation and append `sharing.bill-cancelled.v1` through the same Sharing UoW only after its accounting reversal is durable or a terminal no-effect receipt is proven. Never let a pending worker post after the bill is marked Cancelled.

- [ ] **Step 4: run focused tests**

~~~bash
cargo test --test sharing_accounting
~~~

Expected: PASS.

- [ ] **Step 5 — REFACTOR: narrow Ledger anti-corruption port**

Only contexts::ledger::public is adapted. Sharing's process manager builds typed Sharing intent, not generic postings.

- [ ] **Step 6: commit**

~~~bash
git add src/integration/process_managers/sharing_accounting.rs src/integration/process_managers/mod.rs src/contexts/sharing tests/sharing_accounting.rs
git commit -m "feat(sharing): durably account for shared bills"
~~~

---

## Task 8: Implement partial settlement, linking, and reversal

**Files:**

- Create: src/integration/process_managers/sharing_settlement.rs
- Create: tests/sharing_settlements.rs
- Modify: src/integration/process_managers/mod.rs
- Modify: src/contexts/sharing/application/{commands,handlers,ports}.rs
- Modify: src/contexts/sharing/api/{handlers,routes,dto}.rs
- Modify: src/contexts/sharing/public.rs
- Modify: static/openapi.v2.json

- [ ] **Step 1 — RED: write settlement workflow tests**

Cover manual current-user payment/receipt, linking an existing imported journal, typed reclassification instead of edit, Receivable/Payable reduction without new income/expense, contact-to-contact external settlement, partial amounts, rejection when allocations across contributions/settlements exceed the eligible amount of one Ledger journal, over-settlement rejection under lock, missing/stale BillSplit `expected_version` for settlement creation, missing/stale Settlement `expected_version` for reversal, reversal rejected while settlement accounting is pending/retrying, failed-with-proven-no-effect settlement cancellation without a fake Ledger reversal, crash after Ledger settlement/reversal, once-only reversal, bill revision blocked until all active settlements reverse, and API process status.

The settlement handler and OpenAPI contract must preserve the exact public paths declared in Task 6, including singular `/reversal` for a settlement reversal.

- [ ] **Step 2: run RED**

~~~bash
cargo test --test sharing_settlements -- --nocapture
~~~

Expected: FAIL.

- [ ] **Step 3 — GREEN: implement settlement coordinator**

Use stable keys sharing-settlement:{settlement_id} and sharing-settlement-reversal:{settlement_id}. Aggregate status and process lease state remain separate.

- [ ] **Step 4: run focused tests**

~~~bash
cargo test --test sharing_settlements -- --nocapture
~~~

Expected: PASS.

- [ ] **Step 5 — REFACTOR: prove no income/spending inflation**

Assert Ledger classification/reclassification events mark repayment as settlement/control movement, not new income or duplicate expense.

- [ ] **Step 6: commit**

~~~bash
git add src/integration/process_managers/sharing_settlement.rs src/integration/process_managers/mod.rs src/contexts/sharing static/openapi.v2.json tests/sharing_settlements.rs
git commit -m "feat(sharing): add partial settlement and reversal"
~~~

---

## Task 9: Project obligations and Reporting bill positions

**Files:**

- Create: src/contexts/reporting/infrastructure/sharing_projection.rs
- Create: tests/reporting_sharing.rs
- Modify: src/contexts/reporting/public.rs
- Modify: src/contexts/reporting/application/projectors.rs
- Modify: src/contexts/reporting/infrastructure/mod.rs

- [ ] **Step 1 — RED: write projection/rebuild tests**

Cover positive receivables/negative payables, multiple creditor/debtor obligations, contact-to-contact exclusion from current-user net worth, partial/reversed settlement, revision compensation/replacement, current-user contribution counted once, original Ledger expense counted once, settlement excluded from income/spending, archived-contact display retention, `sharing.bill-cancelled.v1` removing the active bill position while retaining historical cancellation metadata, duplicate cancellation/event delivery producing no second change, and byte-equivalent rebuild/replay including cancellation.

- [ ] **Step 2: run RED**

~~~bash
cargo test --test reporting_sharing
~~~

Expected: FAIL.

- [ ] **Step 3 — GREEN: implement Sharing event consumer**

Consume versioned Sharing events, including `sharing.bill-cancelled.v1`, through Reporting public contract. Do not query sharing schema. Cancellation deterministically closes the active bill-position projection; duplicate delivery is inbox-idempotent and rebuild derives the same closed projection from the event stream.

- [ ] **Step 4: run focused tests**

~~~bash
cargo test --test reporting_sharing
~~~

Expected: PASS.

- [ ] **Step 5 — REFACTOR: no-double-count invariant**

Add one report-level assertion reconciling bill position, Ledger cashflow, and net-worth effects after settlement.

- [ ] **Step 6: commit**

~~~bash
git add src/contexts/reporting tests/reporting_sharing.rs
git commit -m "feat(reporting): project shared bill positions"
~~~

---

## Task 10: Wire V2-only Sharing and prove full crash workflow

**Files:**

- Create: tests/phase5_workflow.rs
- Modify: src/bootstrap/v2.rs
- Modify: src/integration/process_managers/mod.rs
- Modify: static/openapi.v2.json

- [ ] **Step 1 — RED: write full multiple-payer workflow**

Scenario:

1. create external contacts Alice, Bob, Carol;
2. create UAH 1,000 bill;
3. current user contributes 600 across two outgoing Ledger journals;
4. Alice contributes 400 externally;
5. exact/equal shares resolve current user 100, Alice 200, Bob 300, Carol 400;
6. durable accounting creates only current-user-related control effects;
7. assert deterministic obligations and conservation;
8. Bob partially settles via existing imported journal;
9. crash after Ledger reclassification and recover once;
10. reverse settlement;
11. revise bill, reverse/replace its prior accounting exactly once, and prove both revisions/accounting chains remain visible;
12. cancel the revised bill after all settlements are reversed and confirm one `sharing.bill-cancelled.v1`;
13. redeliver the cancellation event and prove the Reporting position is unchanged;
14. archive a contact and preserve historical identity;
15. rebuild Reporting and compare the same cancelled bill position byte-for-byte.

- [ ] **Step 2: run RED**

~~~bash
cargo test --test phase5_workflow -- --nocapture
~~~

Expected: FAIL.

- [ ] **Step 3 — GREEN: wire Sharing in V2 bootstrap**

Register isolated router, event consumers, and bounded process managers. Do not touch default main/router/DATABASE_URL.

- [ ] **Step 4: run Phase 5 suite**

~~~bash
cargo test --test sharing_contacts
cargo test --test sharing_allocations
cargo test --test sharing_persistence
cargo test --test sharing_api
cargo test --test sharing_accounting
cargo test --test sharing_settlements
cargo test --test reporting_sharing
cargo test --test phase5_workflow
~~~

Expected: PASS.

- [ ] **Step 5 — REFACTOR: architecture scan**

~~~bash
cargo test --test context_boundaries
rg -n "(FROM|JOIN|UPDATE|INTO) (ledger|reporting)\\." src/contexts/sharing
~~~

Expected: no foreign-context SQL.

- [ ] **Step 6: commit**

~~~bash
git add src/bootstrap/v2.rs src/integration/process_managers/mod.rs static/openapi.v2.json tests/phase5_workflow.rs
git commit -m "feat(phase5): complete contacts-first bill sharing"
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

1. Sharing schema/invariants.
2. External Contact aggregate.
3. Multiple-payer allocation/obligation engine.
4. Bill revisions and settlement rules.
5. Sharing UoW/persistence.
6. Contacts/BillSplit API/OpenAPI.
7. Durable bill-accounting reclassification.
8. Partial settlement/link/reversal workflow.
9. Reporting bill-position projection.
10. Integrated multiple-payer scenario.

Keep pure allocation arithmetic separate from Ledger orchestration and HTTP mapping so each invariant can be reviewed and reverted independently before cutover.

## Exit criteria

- [ ] Contact is an external user-owned person, never application identity.
- [ ] Multiple contributions are first-class; exact/equal shares persist resolved minor-unit truth.
- [ ] Contribution/share totals equal total and obligations conserve all net positions.
- [ ] Bill currency is immutable; every contribution, share, obligation, and settlement uses it.
- [ ] Current-user contributions support manual payment or several existing outgoing journals.
- [ ] Contact-to-contact obligations remain Sharing facts.
- [ ] Current-user obligations use hidden Ledger contact receivable/payable accounts.
- [ ] Accounting and settlement states survive crash/duplicate delivery.
- [ ] Sharing command receipts enforce same-key/same-hash durable replay and same-key/different-hash conflict with no duplicate effect.
- [ ] Partial settlement rejects overpayment under aggregate lock.
- [ ] Settlement creation and reversal reject missing/stale `expected_version`.
- [ ] Settlements reverse before bill revision/cancellation.
- [ ] Cancellation emits one versioned event; Reporting handles duplicate delivery and exact rebuild without retaining an active bill position.
- [ ] Reporting does not double count payment, share, settlement, income, or expense.
- [ ] Parallel V2 OpenAPI validates; default runtime/API/DATABASE_URL remain legacy.
- [ ] Blank migration, domain, DB, API, workflow, format, clippy, and boundary tests pass.

## Out of scope

- Registered-user invitations or contact claiming.
- Contact merge/deduplication workflows.
- Percentage/weighted or cross-currency shares.
- Household/group authorization.
- External payment requests/collection.
- Legacy-data backfill.
- Mounting V2 routes or switching DATABASE_URL before Phase 8.
