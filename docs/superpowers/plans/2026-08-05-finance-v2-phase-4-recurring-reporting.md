# Finance V2 Phase 4 — Mail, Recurring, and Reporting Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Keep checkbox state in this document and execute tasks in order.

**Goal:** Rebuild encrypted Gmail ingestion, recurring-subscription inventory and matching, external FX reference observations, and financial reporting on the blank Finance V2 database without reading Ledger-private tables or preserving legacy database rows.

**Dependencies:** Phases 1–3 are integrated for the Phase 4 exit gate. The shared kernel, V2 database guard, integration outbox/inbox and lease primitives, Reference Data currency facade, Ledger event feed, Ledger annotation command, Ledger reversal events, Ledger public candidate/export façades, and Banking observation events are stable. Mail, Recurring, and Reference Data FX work may begin after Phase 2 on isolated branches, but migrations `0005`–`0008` land only after `0004` and are verified together on a fresh database.

**Architecture:** Mail, Recurring, Reference Data, and Reporting remain separate bounded contexts. Mail owns encrypted Gmail connections, OAuth state, immutable messages, append-only fetch/parse attempts, cursors, and leases. It publishes immutable receipt evidence. Recurring owns Subscription and ChargeEvidence aggregates, lifecycle, a local Ledger-candidate projection, append-only allocated matches/rejections, and categorization requests. Reference Data owns immutable external FX observations and leased source synchronization. Reporting owns only rebuildable projections and read APIs, including a local FX-rate projection for deterministic base-currency views. Cross-context consumers use versioned event envelopes, inbox deduplication, and durable process managers; no context executes SQL against another context's schema.

**Tech Stack:** Rust 2024, Axum 0.8, SQLx 0.8/PostgreSQL 16, Tokio, rust_decimal, Chrono, Serde, UUID, Reqwest, the approved V2 credential-encryption port, and Testcontainers.

**Spec:** docs/superpowers/specs/2026-08-05-finance-ddd-v2-design.md

---

## Non-negotiable decisions

- V2 uses src/infrastructure/migrations_v2. Never edit or append to src/infrastructure/migrations.
- The database is blank. There is no legacy subscription, charge, Gmail credential, transaction, or reporting backfill.
- Users reconnect Gmail. Tokens and OAuth verifiers are encrypted-only and never logged.
- Phase 4 extends only the parallel V2 bootstrap and static/openapi.v2.json. Default runtime, API router, and DATABASE_URL remain legacy until Phase 8.
- The replacement API is unversioned when promoted in Phase 8. Do not add /v2 route prefixes inside the V2 router.
- Mail publishes evidence; it does not create subscriptions or inspect Ledger.
- Recurring never reads Ledger tables. Its candidates are a local projection of versioned Ledger events/public exports.
- One ChargeEvidence may match one or more Ledger journals through allocated Money; there is no single legacy transaction_id relationship.
- Matches, rejections, and unmatches are append-only.
- Categorization uses Ledger's public annotation command with a derived idempotency key.
- Reference Data owns positive Decimal FX-rate observations, source revisions, and NBU synchronization. Rates never mutate or auto-price a posted Ledger transfer.
- Reporting consumes `reference-data.fx-observed.v1` into a local projection. Missing historical rates remain explicit; it never queries Reference Data tables or silently substitutes today's rate.
- Reporting has no financial command port and never repairs Ledger.
- Timers only wake durable leases/processes. Cursors and attempts are persisted.

## Frozen Phase 4 HTTP and command-receipt contract

All paths below are composed only into `src/api/v2.rs` during this phase and become unversioned root paths at the Phase 8 cutover. Do not introduce aliases, `/v2` prefixes, or legacy compatibility handlers.

Mail exposes exactly:

```text
POST /me/email-connections/gmail/oauth/start
GET  /oauth/gmail/callback
GET  /me/email-connections
GET  /me/email-connections/{connection_id}/status
POST /me/email-connections/{connection_id}/disconnect
POST /me/email-connections/{connection_id}/resync
```

The authenticated `oauth/start`, `disconnect`, and `resync` commands require `Idempotency-Key`. Disconnect and resync bodies require `expected_version`; an `oauth/start` that replaces an existing connection requires both `connection_id` and `expected_version`, which are integrity-protected in the OAuth state and rechecked by the callback. The callback is the browser/provider protocol exception to the header rule: its durable replay identity is the hashed, single-use OAuth state. It records a callback receipt before exchanging or accepting tokens, returns the already-recorded terminal redirect for the same state/code digest, and rejects a reused state with a different code digest. Resync returns `202 Accepted` with a durable job/status reference; status reads never trigger work.

Recurring exposes exactly:

```text
GET   /subscriptions
GET   /subscriptions/{subscription_id}
PATCH /subscriptions/{subscription_id}
GET   /subscriptions/{subscription_id}/charges
GET   /subscriptions/forecast
POST  /subscription-charges/{charge_evidence_id}/matches
POST  /subscription-charges/{charge_evidence_id}/rejections
POST  /subscription-charges/{charge_evidence_id}/matches/{match_id}/unmatches
```

The `PATCH`, match, rejection, and unmatch commands require `Idempotency-Key` and an `expected_version` field in the request body. `PATCH` fences `Subscription.version`. Match and rejection fence the per-evidence `ChargeMatching.version`; unmatch also fences that same aggregate version while validating the immutable referenced `MatchRecord` and its once-only unmatch relation. Charge detail reads return `matching_version`; first decision uses version `0`. A match body contains one or more `{ journal_entry_id, amount }` allocations; amounts are decimal-string `Money` in the evidence currency. Rejection and unmatch append facts rather than deleting a prior fact.

Reporting exposes exactly these read-only paths:

```text
GET /reports/balance-history
GET /reports/cashflow
GET /reports/spending
GET /reports/liabilities
GET /reports/reconciliations
GET /reports/recurring
GET /reports/net-worth
```

Date-ranged reports accept `from`, `to`, and `timezone`; currency-converting reports additionally accept `base_currency`. Every response carries `as_of`, projection sequence, lag, source currency, and an explicit missing-rate status where conversion is incomplete.

Reference Data extends its Phase 1 currency router with exactly `GET /fx-rates?base_currency={code}&quote_currency={code}&as_of={timestamp}`. The read returns the selected immutable source observation/provenance, exact decimal rate, effective/recorded times, inversion/cross-rate derivation, and a typed missing-rate result. It has no mutation side effect; existing `GET /currencies` and `GET /currencies/{code}` operations remain unchanged.

Mail and Recurring each own a durable command-receipt table in their own schema: `mail.command_receipts` in migration `0005` and `recurring.command_receipts` in migration `0006`. The primary key is `(user_id, command_scope, idempotency_key)`. Each row stores the semantic command name, target ID when present, canonical request hash (including scope, target, normalized body, and authenticated user), processing/terminal status, stable HTTP status and response body, resulting aggregate/version IDs, and timestamps. Receipt claim, aggregate changes, audit facts, outbox events, and terminal receipt result commit in the context's UoW. Concurrent use of the same scoped key and hash waits/reloads and returns the recorded response with no second effect; use of the same scoped key with any different hash, target, or user-visible payload returns HTTP `409 idempotency_conflict`. Keys in distinct documented command scopes are independent. Terminal domain failures are replayed consistently. Neither context relies on a Ledger receipt table or an in-memory cache.

## Entry gate

- [x] ledger.journal-posted.v1, ledger.journal-reversed.v1, and ledger.annotation-changed.v1 are frozen.
- [x] Ledger exposes a tenant-safe candidate/export façade.
- [x] Ledger annotation is idempotent and audit aware.
- [x] V2 inbox consumption deduplicates by consumer plus event ID and rejects unknown major versions.
- [x] V2 tests use only the blank V2 migrator.

Run:

~~~bash
SQLX_OFFLINE=true cargo test --test v2_migrations -- --nocapture
cargo test --test context_boundaries
cargo test --test ledger_public_contracts
~~~

Expected: PASS.

---

## File map

| File | Action | Responsibility |
|---|---|---|
| src/infrastructure/migrations_v2/0005_mail.sql | Create | Mail schema, encrypted credentials, immutable messages, attempts, leases, cursors, OAuth callback receipts, and context-owned command receipts |
| src/infrastructure/migrations_v2/0006_recurring.sql | Create | Subscription/evidence/match facts, candidate projection, Recurring inbox/outbox, and context-owned command receipts |
| src/infrastructure/migrations_v2/0007_reference_fx.sql | Create | Immutable FX observations, source revisions, and durable sync cursor/state owned by Reference Data |
| src/infrastructure/migrations_v2/0008_reporting.sql | Create | Rebuildable Reporting projections (including empty bill/loan/Portfolio extension tables), failures, consumed events, checkpoints |
| src/contexts/reference_data/domain.rs | Modify | Add exact positive ExchangeRate/observation/source rules |
| src/contexts/reference_data/mod.rs | Modify | Export the context's public facade and isolated API only |
| src/contexts/reference_data/application.rs | Modify | FX observation ingestion, sync, and query use cases |
| src/contexts/reference_data/infrastructure.rs | Modify | FX repository and submodule wiring |
| src/contexts/reference_data/infrastructure/nbu.rs | Create | NBU wire adapter, normalization, redacted failures |
| src/contexts/reference_data/infrastructure/fx_repository.rs | Create | Immutable observation/latest-rate/sync-state persistence |
| src/contexts/reference_data/public.rs | Modify | Publish rate queries and versioned observation events |
| src/contexts/reference_data/api/dto.rs | Create | Decimal-string FX read DTOs |
| src/contexts/reference_data/api/handlers.rs | Create | Tenant-neutral/reference rate query handlers |
| src/contexts/reference_data/api/routes.rs | Create | Isolated V2 FX read router |
| src/contexts/reference_data/api/mod.rs | Create | API exports |
| src/contexts/mail/mod.rs | Modify Phase 1 skeleton | Mail exports |
| src/contexts/mail/public.rs | Modify Phase 1 skeleton | Mail commands/read DTOs and receipt-evidence events |
| src/contexts/mail/domain/connection.rs | Create | GmailConnection aggregate |
| src/contexts/mail/domain/message.rs | Create | Immutable SourceMessage aggregate |
| src/contexts/mail/domain/attempt.rs | Create | Append-only fetch/parse attempt facts |
| src/contexts/mail/domain/error.rs | Create | Stable Mail errors |
| src/contexts/mail/domain/mod.rs | Create | Domain exports |
| src/contexts/mail/application/commands.rs | Create | Connect, disconnect, resync DTOs plus canonical request hashing/receipt contracts |
| src/contexts/mail/application/handlers.rs | Create | Mail UoW orchestration |
| src/contexts/mail/application/queries.rs | Create | Connection/sync/evidence reads |
| src/contexts/mail/application/ports.rs | Create | UoW, OAuth, Gmail, encryption, outbox ports |
| src/contexts/mail/application/mod.rs | Create | Application exports |
| src/contexts/mail/infrastructure/repository.rs | Create | Mail aggregate persistence |
| src/contexts/mail/infrastructure/unit_of_work.rs | Create | SQLx Mail UoW |
| src/contexts/mail/infrastructure/oauth.rs | Create | OAuth state/encrypted token adapter |
| src/contexts/mail/infrastructure/gmail.rs | Create | Gmail fetch adapter |
| src/contexts/mail/infrastructure/parsers/mod.rs | Create | Parser registry |
| src/contexts/mail/infrastructure/parsers/google_play.rs | Create | Google Play parser |
| src/contexts/mail/infrastructure/parsers/apple.rs | Create | Apple parser |
| src/contexts/mail/infrastructure/parsers/netflix.rs | Create | Netflix parser |
| src/contexts/mail/infrastructure/mod.rs | Create | Infrastructure exports |
| src/contexts/mail/api/dto.rs | Create | Gmail connection HTTP DTOs |
| src/contexts/mail/api/handlers.rs | Create | Gmail/OAuth/resync handlers |
| src/contexts/mail/api/routes.rs | Create | Isolated V2 Mail router |
| src/contexts/mail/api/mod.rs | Create | API exports |
| src/contexts/recurring/mod.rs | Modify Phase 1 skeleton | Recurring exports |
| src/contexts/recurring/public.rs | Modify Phase 1 skeleton | Recurring commands, reads, events |
| src/contexts/recurring/domain/subscription.rs | Create | Subscription aggregate/lifecycle |
| src/contexts/recurring/domain/charge_evidence.rs | Create | ChargeEvidence aggregate |
| src/contexts/recurring/domain/match_record.rs | Create | Allocated match/rejection facts |
| src/contexts/recurring/domain/error.rs | Create | Stable Recurring errors |
| src/contexts/recurring/domain/mod.rs | Create | Domain exports |
| src/contexts/recurring/application/commands.rs | Create | Inventory/match/lifecycle DTOs plus canonical request hashing/receipt contracts |
| src/contexts/recurring/application/handlers.rs | Create | Recurring UoW orchestration |
| src/contexts/recurring/application/queries.rs | Create | Inventory/charges/forecast reads |
| src/contexts/recurring/application/ports.rs | Create | Repositories, UoW, Ledger annotation port |
| src/contexts/recurring/application/mod.rs | Create | Application exports |
| src/contexts/recurring/infrastructure/repository.rs | Create | Aggregate persistence |
| src/contexts/recurring/infrastructure/unit_of_work.rs | Create | SQLx Recurring UoW |
| src/contexts/recurring/infrastructure/ledger_projection.rs | Create | Local Ledger candidate projection |
| src/contexts/recurring/infrastructure/queries.rs | Create | Read-side SQL |
| src/contexts/recurring/infrastructure/mod.rs | Create | Infrastructure exports |
| src/contexts/recurring/api/dto.rs | Create | Subscription/charge DTOs |
| src/contexts/recurring/api/handlers.rs | Create | Inventory/match handlers |
| src/contexts/recurring/api/routes.rs | Create | Isolated V2 Recurring router |
| src/contexts/recurring/api/mod.rs | Create | API exports |
| src/integration/process_managers/recurring_match.rs | Create | Durable matching/annotation coordinator |
| src/integration/process_managers/mod.rs | Modify | Register Recurring matcher |
| src/contexts/reporting/mod.rs | Modify Phase 1 skeleton | Reporting exports |
| src/contexts/reporting/public.rs | Modify Phase 1 skeleton | Report queries and consumer contract |
| src/contexts/reporting/application/projectors.rs | Create | Versioned event projectors |
| src/contexts/reporting/application/queries.rs | Create | Financial report queries |
| src/contexts/reporting/application/ports.rs | Create | Projection UoW/read ports |
| src/contexts/reporting/application/mod.rs | Create | Application exports |
| src/contexts/reporting/infrastructure/projections.rs | Create | Projection persistence/rebuild |
| src/contexts/reporting/infrastructure/queries.rs | Create | Read SQL |
| src/contexts/reporting/infrastructure/mod.rs | Create | Infrastructure exports |
| src/contexts/reporting/api/dto.rs | Create | Report DTOs |
| src/contexts/reporting/api/handlers.rs | Create | Report handlers |
| src/contexts/reporting/api/routes.rs | Create | Isolated V2 Reporting router |
| src/contexts/reporting/api/mod.rs | Create | API exports |
| src/contexts/mod.rs | Modify | Export Mail, Recurring, Reporting |
| src/api/v2.rs | Modify | Compose Mail, Recurring, and Reporting routers into the isolated replacement router |
| src/bootstrap/v2.rs | Modify | Construct V2-only contexts/consumers/workers |
| static/openapi.v2.json | Modify | Add future unversioned Mail/Recurring/Reporting routes |
| tests/mail_domain.rs | Create | Mail aggregate tests |
| tests/mail_persistence.rs | Create | Constraints/encryption/lease tests |
| tests/mail_sync.rs | Create | Gmail retry/cursor/parser tests |
| tests/recurring_domain.rs | Create | Subscription/evidence/match tests |
| tests/recurring_matching.rs | Create | Projection/process tests |
| tests/recurring_api.rs | Create | Parallel V2 API tests |
| tests/reference_fx.rs | Create | Rate value/schema/repository/API tests |
| tests/reference_fx_sync.rs | Create | NBU normalization/retry/lease/replay tests |
| tests/reporting_projections.rs | Create | Exactly-once/rebuild tests |
| tests/reporting_api.rs | Create | Parallel V2 report API tests |
| tests/phase4_workflow.rs | Create | Restart/duplicate/reversal workflow |

Paths assume earlier phases established src/bootstrap/v2.rs, static/openapi.v2.json, and process-manager registration. Reuse their exact helper names rather than create duplicate abstractions.

---

## Task 1: Create the Mail schema

**Files:**

- Create: src/infrastructure/migrations_v2/0005_mail.sql
- Create: tests/mail_persistence.rs

- [ ] **Step 1 — RED: write fresh-database invariant tests**

Cover encrypted-only connection credentials, monotonically increasing credential/sync generation, hashed/single-use OAuth state, immutable source messages, append-only fetch/parse attempts, durable generation-bound cursor/job, lease owner/expiry/fencing token, attempts/retry time, composite tenant constraints, valid states, context-local inbox/outbox relationships, `mail.command_receipts`, and durable OAuth callback receipts. Prove the command-receipt uniqueness and canonical-hash constraints independently of HTTP.

- [ ] **Step 2: run and capture RED**

~~~bash
SQLX_OFFLINE=true cargo test --test mail_persistence schema_ -- --nocapture
~~~

Expected: FAIL because mail schema does not exist.

- [ ] **Step 3 — GREEN: add migration 0005**

Use schema mail, TIMESTAMPTZ, bounded columns, composite user keys, immutable-fact triggers, and worker indexes. Add `mail.command_receipts` with the frozen receipt contract and a callback-receipt table keyed by OAuth state digest. A sync claim/cursor/page commit includes both fenced lease token and connection credential generation; disconnect/replacement increments the generation so stale claims cannot commit. Do not create foreign-context foreign keys.

- [ ] **Step 4: run focused tests**

~~~bash
SQLX_OFFLINE=true cargo test --test mail_persistence schema_ -- --nocapture
~~~

Expected: PASS.

- [ ] **Step 5 — REFACTOR: inspect context ownership**

~~~bash
rg -n "REFERENCES (ledger|banking|recurring|reporting)\\." src/infrastructure/migrations_v2/0005_mail.sql
~~~

Expected: no matches.

- [ ] **Step 6: commit**

~~~bash
git add src/infrastructure/migrations_v2/0005_mail.sql tests/mail_persistence.rs
git commit -m "feat(mail): add v2 mail schema"
~~~

---

## Task 2: Implement Mail aggregates and encrypted persistence

**Files:**

- Create: src/contexts/mail/domain/{connection,message,attempt,error,mod}.rs
- Create: src/contexts/mail/application/{commands,handlers,queries,ports,mod}.rs
- Create: src/contexts/mail/infrastructure/{repository,unit_of_work,mod}.rs
- Modify: src/contexts/mail/{mod,public}.rs
- Create: tests/mail_domain.rs
- Modify: tests/mail_persistence.rs
- Modify: src/contexts/mod.rs

- [ ] **Step 1 — RED: write aggregate/UoW tests**

Prove lifecycle transitions, redacted secrets, message identity by connection/provider ID/payload hash, retained revisions, append-only attempts, atomic aggregate plus outbox plus command receipt, optimistic conflicts, and cross-user rollback. Prove the same user/scope/key plus the same canonical hash replays the stored response, while the same user/scope/key with a different target or payload maps to `409 idempotency_conflict` without a second effect; a distinct documented command scope is independent.

- [ ] **Step 2: run RED**

~~~bash
cargo test --test mail_domain
cargo test --test mail_persistence aggregate_
~~~

Expected: FAIL.

- [ ] **Step 3 — GREEN: implement aggregate repositories and Mail UoW**

One Mail transaction covers aggregate, attempt, audit, context-owned command receipt, and outbox. Implement a unique-key claim/reload protocol that is correct under concurrent requests; do not use process memory as the idempotency authority.

- [ ] **Step 4: run focused tests**

~~~bash
cargo test --test mail_domain -- --nocapture
cargo test --test mail_persistence aggregate_ -- --nocapture
~~~

Expected: PASS.

- [ ] **Step 5 — REFACTOR: enforce purity**

~~~bash
rg -n "crate::contexts::(ledger|recurring|reporting)::(domain|application|infrastructure)" src/contexts/mail
rg -n "sqlx|axum|reqwest" src/contexts/mail/domain
~~~

Expected: no matches.

- [ ] **Step 6: commit**

~~~bash
git add src/contexts/mail src/contexts/mod.rs tests/mail_domain.rs tests/mail_persistence.rs
git commit -m "feat(mail): add immutable mail aggregates"
~~~

---

## Task 3: Add Gmail reconnect API and durable synchronization

**Files:**

- Create: src/contexts/mail/infrastructure/{oauth,gmail}.rs
- Create: src/contexts/mail/api/{dto,handlers,routes,mod}.rs
- Create: tests/mail_sync.rs
- Modify: src/contexts/mail/application/{handlers,ports}.rs
- Modify: src/contexts/mail/infrastructure/{repository,mod}.rs
- Modify: src/contexts/mail/public.rs
- Modify: src/api/v2.rs
- Modify: src/bootstrap/v2.rs
- Modify: static/openapi.v2.json
- Modify: tests/mail_persistence.rs

- [ ] **Step 1 — RED: write OAuth/API/worker tests**

Cover every frozen Mail route and method, hashed expiring single-use state, encrypted verifier/tokens, no secret logs, reconnect audit, strict ownership, and status reads with no side effect. Require `Idempotency-Key` on `oauth/start`, disconnect, and resync; require and reject stale `expected_version` for disconnect, resync, and replacement starts; prove same-key/same-hash response replay and same-key/different-hash HTTP 409. Cover callback same-state/same-code replay, same-state/different-code rejection, durable resync returning `202` plus a status reference, exclusive/reclaimable lease, cursor advancement only after complete page commit, retry/backoff, NeedsReauth, duplicate messages, and restart recovery. Exercise disconnect during an old-generation fetch and OAuth replacement during an old-generation fetch, in both before-response and before-page-commit orderings; the stale worker must not persist messages/evidence, advance/reset the new generation's cursor, restore revoked credentials, or change connection state.

- [ ] **Step 2: run RED**

~~~bash
cargo test --test mail_persistence -- --nocapture
cargo test --test mail_sync -- --nocapture
~~~

Expected: FAIL.

- [ ] **Step 3 — GREEN: implement OAuth, Gmail adapter, routes, and worker**

Implement only the frozen Mail paths in this plan and compose the Mail router in `src/api/v2.rs`. The public OAuth redirect lands on `GET /oauth/gmail/callback`; all other routes are authenticated. Seal the optional replacement connection ID/version into state, persist callback and command receipts, and map receipt hash conflicts to `409 idempotency_conflict`. Every claimed job captures connection version, credential generation, and lease fencing token; the page UoW rechecks all three plus active lifecycle before writing messages/evidence/cursor. Disconnect and successful OAuth replacement increment generation and fence/cancel old jobs. Provider calls occur outside Mail SQL transactions, and a stale response is discarded/redacted. The timer wakes persisted work only. Do not touch the default legacy router.

- [ ] **Step 4: run focused tests**

~~~bash
cargo test --test mail_persistence -- --nocapture
cargo test --test mail_sync -- --nocapture
cargo test --test openapi_v2 -- --nocapture
~~~

Expected: PASS.

- [ ] **Step 5 — REFACTOR: secret and lock audit**

Block the Gmail fake and prove no SQL transaction remains open. Search logs for token/code/verifier/body output.

- [ ] **Step 6: commit**

~~~bash
git add src/contexts/mail src/api/v2.rs src/bootstrap/v2.rs static/openapi.v2.json tests/mail_persistence.rs tests/mail_sync.rs
git commit -m "feat(mail): add encrypted durable Gmail sync"
~~~

---

## Task 4: Port receipt parsers and publish immutable evidence

**Files:**

- Create: src/contexts/mail/infrastructure/parsers/{mod,google_play,apple,netflix}.rs
- Modify: src/contexts/mail/application/{ports,handlers}.rs
- Modify: src/contexts/mail/public.rs
- Modify: tests/mail_sync.rs
- Reuse: tests/fixtures/receipts/**

- [ ] **Step 1 — RED: write parser characterization tests**

Use every existing fixture. Preserve Google Play renewal/one-time/div/UAH, Apple renewal/refund/table, Netflix renewal/cancellation, amount/currency, merchant identity, evidence kind, charged time, and provenance. Add malformed input, duplicate parse, parser panic isolation, and parser-version revision tests.

- [ ] **Step 2: run RED**

~~~bash
cargo test --test mail_sync parser_
~~~

Expected: FAIL.

- [ ] **Step 3 — GREEN: implement parser adapters**

Parsing commits append-only ParseAttempt, immutable ReceiptEvidence, and mail.receipt-evidence-recorded.v1 in one Mail UoW. Provider structs remain infrastructure-private.

- [ ] **Step 4: run focused tests**

~~~bash
cargo test --test mail_sync parser_ -- --nocapture
~~~

Expected: PASS.

- [ ] **Step 5 — REFACTOR: verify provider-neutral public evidence**

The public event contains normalized evidence and provenance, not Gmail or HTML parser DTOs.

- [ ] **Step 6: commit**

~~~bash
git add src/contexts/mail tests/mail_sync.rs
git commit -m "feat(mail): publish immutable receipt evidence"
~~~

---

## Task 5: Create the Recurring schema and domain

**Files:**

- Create: src/infrastructure/migrations_v2/0006_recurring.sql
- Create: src/contexts/recurring/domain/{subscription,charge_evidence,charge_matching,match_record,error,mod}.rs
- Modify: src/contexts/recurring/{mod,public}.rs
- Create: tests/recurring_domain.rs
- Create: tests/recurring_matching.rs
- Modify: src/contexts/mod.rs

- [ ] **Step 1 — RED: write schema/domain tests**

Cover Subscription lifecycle and cadence, immutable ChargeEvidence provenance/kind, the per-evidence `ChargeMatching` decision-stream aggregate/version, one match with several allocated journals, exact allocated totals, partial-match state, append-only rejection/unmatch, concurrent match-versus-rejection and match-versus-unmatch version races, local candidate identity, composite tenant safety, valid states, immutable facts, `recurring.command_receipts`, and no cross-context foreign keys. Prove the receipt table's user/scope/key uniqueness, canonical request hash, terminal response, and aggregate-version fields.

- [ ] **Step 2: run RED**

~~~bash
SQLX_OFFLINE=true cargo test --test recurring_domain -- --nocapture
SQLX_OFFLINE=true cargo test --test recurring_matching schema_ -- --nocapture
~~~

Expected: FAIL.

- [ ] **Step 3 — GREEN: add migration 0006 and aggregates**

Use schema recurring. Add the context-owned `recurring.command_receipts` table from the frozen contract. Keep ChargeEvidence separate from Subscription so no aggregate owns an unbounded collection; `ChargeMatching` owns only the versioned decision stream/projection for one evidence ID, while immutable MatchRecord/rejection/unmatch facts remain append-only children. Expected charges never post money automatically.

- [ ] **Step 4: run focused tests**

~~~bash
SQLX_OFFLINE=true cargo test --test recurring_domain -- --nocapture
SQLX_OFFLINE=true cargo test --test recurring_matching schema_ -- --nocapture
~~~

Expected: PASS.

- [ ] **Step 5 — REFACTOR: explicit transitions and immutable facts**

Methods return events/errors; no public field mutation or SQL upsert bypasses aggregate rules.

- [ ] **Step 6: commit**

~~~bash
git add src/infrastructure/migrations_v2/0006_recurring.sql src/contexts/recurring src/contexts/mod.rs tests/recurring_domain.rs tests/recurring_matching.rs
git commit -m "feat(recurring): add v2 recurring domain and schema"
~~~

---

## Task 6: Persist Recurring and consume Mail evidence

**Files:**

- Create: src/contexts/recurring/application/{commands,handlers,queries,ports,mod}.rs
- Create: src/contexts/recurring/infrastructure/{repository,unit_of_work,queries,mod}.rs
- Modify: src/contexts/recurring/public.rs
- Modify: tests/recurring_matching.rs

- [ ] **Step 1 — RED: write UoW/inbox tests**

Prove aggregate plus outbox plus command-receipt atomicity, optimistic rollback, concurrent same-key/same-hash replay, same-key/different-hash conflict, once-only Mail evidence consumption, dead-letter unknown major versions, crash-safe acknowledgement, no Mail/Ledger SQL, and merchant aggregation through domain methods.

- [ ] **Step 2: run RED**

~~~bash
cargo test --test recurring_matching -- --nocapture
~~~

Expected: FAIL.

- [ ] **Step 3 — GREEN: implement Recurring UoW and Mail consumer**

Consume public versioned evidence only. Store Mail evidence ID plus user as opaque provenance. Implement Recurring's receipt claim/reload in its own UoW so a terminal response and aggregate/outbox effects cannot diverge.

- [ ] **Step 4: run focused tests**

~~~bash
cargo test --test recurring_matching -- --nocapture
~~~

Expected: PASS.

- [ ] **Step 5 — REFACTOR: split aggregate and query stores**

Inventory/forecast SQL stays out of repositories.

- [ ] **Step 6: commit**

~~~bash
git add src/contexts/recurring tests/recurring_matching.rs
git commit -m "feat(recurring): ingest receipt evidence durably"
~~~

---

## Task 7: Build local Ledger candidates and durable allocated matching

**Files:**

- Create: src/contexts/recurring/infrastructure/ledger_projection.rs
- Create: src/integration/process_managers/recurring_match.rs
- Modify: src/integration/process_managers/mod.rs
- Modify: src/contexts/recurring/application/ports.rs
- Modify: src/contexts/recurring/public.rs
- Modify: tests/recurring_matching.rs

- [ ] **Step 1 — RED: write projector/matcher/process tests**

Cover posted/reversed/annotated Ledger events, duplicate/out-of-order delivery, local candidates, multi-journal allocation, ambiguity, remembered rejection, allocation overcommit locking, atomic match/outbox, idempotent Ledger annotation, crash after annotation commit, and visible terminal/transient process states. Test an unmatch request while categorization is pending/in flight, unmatch after Ledger annotation commits but before Recurring acknowledgment, compensation after a confirmed annotation, and a later user annotation edit that must not be overwritten.

- [ ] **Step 2: run RED**

~~~bash
cargo test --test recurring_matching -- --nocapture
~~~

Expected: FAIL.

- [ ] **Step 3 — GREEN: implement projection and process manager**

Use a narrow adapter over `contexts::ledger::public`. Persist score inputs, decision source, prior Ledger annotation snapshot/version, produced annotation version, and process generation. Serialize categorization by MatchRecord. While its categorization state is `Pending`/`RetryDue`/invocation-uncertain, unmatch returns `409 categorization_pending` with the process reference and appends no unmatch fact; recovery first resolves the derived Ledger idempotency receipt, so an accepted unmatch can never be followed by a stale original annotation call. After `Posted`, unmatch appends under the current `ChargeMatching.version` and starts an idempotent compensation: recompute the desired category from remaining active matches, or restore the saved pre-match category if none remain, but only with the exact Ledger annotation version produced by the match. A newer user edit yields visible `CompensationSkippedNewerAnnotation`/review-required rather than being overwritten. A terminal proven-no-effect categorization may be unmatched without compensation. Expose `Pending`, `Posted`, `RetryDue`, `TerminalNoEffect`, `Compensating`, `Compensated`, and review-required states.

- [ ] **Step 4: run focused tests**

~~~bash
cargo test --test recurring_matching -- --nocapture
~~~

Expected: PASS.

- [ ] **Step 5 — REFACTOR: extract pure explainable scoring**

No SQL, event bus, or Ledger type belongs in scoring/allocation functions.

- [ ] **Step 6: commit**

~~~bash
git add src/contexts/recurring src/integration/process_managers tests/recurring_matching.rs
git commit -m "feat(recurring): durably match allocated ledger charges"
~~~

---

## Task 8: Expose Recurring inventory and matching API

**Files:**

- Create: src/contexts/recurring/api/{dto,handlers,routes,mod}.rs
- Create: tests/recurring_api.rs
- Modify: src/api/v2.rs
- Modify: src/bootstrap/v2.rs
- Modify: static/openapi.v2.json

- [ ] **Step 1 — RED: write parallel V2 API tests**

Cover every frozen Recurring path and method: list, get, charges, forecast, `PATCH /subscriptions/{subscription_id}`, create match/rejection, and append an unmatch. Require `expected_version` and `Idempotency-Key` on every command. Assert the exact version mapping frozen above, including concurrent match/rejection/unmatch races and `409 categorization_pending` before an unmatch can be accepted. Prove a concurrent same-scope/key/same-canonical-hash retry returns the byte-equivalent stored status/body with one effect, while reuse in that scope with a different body or target returns `409 idempotency_conflict`; prove a different documented command scope is independent. Cover stale-version conflicts, manual multi-journal allocations, tenant isolation, decimal-string Money, and absence of legacy mutable `transaction_id`.

- [ ] **Step 2: run RED**

~~~bash
cargo test --test recurring_api
~~~

Expected: FAIL.

- [ ] **Step 3 — GREEN: implement isolated V2 routes and OpenAPI**

Implement exactly the Recurring route table frozen above and compose it only in `src/api/v2.rs`, never the default router. Translate stale versions and receipt hash conflicts to distinct stable 409 error codes. Phase 8 promotes the V2 router and OpenAPI.

- [ ] **Step 4: run focused tests**

~~~bash
cargo test --test recurring_api -- --nocapture
cargo test --test openapi_v2 -- --nocapture
~~~

Expected: PASS.

- [ ] **Step 5 — REFACTOR: keep translation at the edge**

Axum/OpenAPI DTOs do not enter domain or ports.

- [ ] **Step 6: commit**

~~~bash
git add src/contexts/recurring/api src/api/v2.rs src/bootstrap/v2.rs static/openapi.v2.json tests/recurring_api.rs
git commit -m "feat(api): add parallel recurring API"
~~~

---

## Task 9: Port Reference Data FX observations and durable NBU sync

**Files:**

- Create: src/infrastructure/migrations_v2/0007_reference_fx.sql
- Modify: src/contexts/reference_data/{mod,domain,application,infrastructure,public}.rs
- Create: src/contexts/reference_data/infrastructure/{nbu,fx_repository}.rs
- Modify/extend: src/contexts/reference_data/api/{dto,handlers,routes,mod}.rs
- Modify: src/api/v2.rs
- Modify: src/bootstrap/v2.rs
- Modify: static/openapi.v2.json
- Create: tests/reference_fx.rs
- Create: tests/reference_fx_sync.rs
- Modify: tests/v2_migrations.rs

- [ ] **Step 1 — RED: write rate, persistence, adapter, and worker tests**

Cover:

- a positive bounded Decimal `ExchangeRate` with distinct base/quote currencies, explicit scale, effective date/time, source, observed/recorded times, source revision, and content digest;
- a canonical meaning of `base -> quote` (one unit of base equals `rate` units of quote), exact inversion/triangulation policies, and named rounding only when producing Money;
- immutable source observations, duplicate same-revision/same-digest replay, conflicting digest quarantine, and latest-as-of ordering by `(effective_at, source_priority, observed_at, sequence, id)`;
- NBU normalization as foreign currency -> UAH without floating point, raw-body logging, or provider types outside the adapter;
- timeout/429/5xx retry, bounded error, fenced lease, crash/restart, date cursor, configurable backfill window, and no cursor advance before a fetched date commits completely;
- `reference-data.fx-observed.v1` outbox atomicity and replay;
- the exact `GET /fx-rates?base_currency={code}&quote_currency={code}&as_of={timestamp}` contract plus regression coverage that Phase 1 currency operations remain mounted and unchanged;
- no automatic Ledger transfer mutation or pricing side effect.

- [ ] **Step 2: run and capture RED**

~~~bash
SQLX_OFFLINE=true cargo test --test reference_fx -- --nocapture
cargo test --test reference_fx_sync -- --nocapture
~~~

Expected: FAIL because migration `0007` and the V2 FX behavior do not exist.

- [ ] **Step 3 — GREEN: implement the owned schema, adapter, and leased sync**

Create append-only `reference_data.fx_observations`, conflict/quarantine facts, and durable per-source sync state. Keep generic attempts/leases in the Phase 1 integration runtime where possible. The NBU adapter persists normalized observations before advancing its date cursor and publishes one versioned event per accepted observation. A retry of the same source fact returns the existing result.

Expose rate lookup through `reference_data::public` and compose its read-only router only in `src/api/v2.rs`. Reporting consumers receive events; they never query `reference_data.*`. Ledger FX transfer commands continue to require explicit source/target Money and merely record the user-confirmed implied rate.

- [ ] **Step 4: run focused tests**

~~~bash
SQLX_OFFLINE=true cargo test --test reference_fx -- --nocapture
cargo test --test reference_fx_sync -- --nocapture
cargo test --test v2_migrations -- --nocapture
cargo test --test openapi_v2 -- --nocapture
~~~

Expected: PASS.

- [ ] **Step 5 — REFACTOR: enforce Reference Data ownership and safe logging**

~~~bash
rg -n "f32|f64|raw(_body|_payload)?|FROM (ledger|reporting)\\.|JOIN (ledger|reporting)\\." src/contexts/reference_data
cargo test --test context_boundaries -- --nocapture
~~~

Expected: no floating-point rates, raw financial body logging, foreign-schema SQL, or boundary failures. Inspect legitimate fixture/type-name matches manually.

- [ ] **Step 6: commit**

~~~bash
git add src/infrastructure/migrations_v2/0007_reference_fx.sql src/contexts/reference_data src/api/v2.rs src/bootstrap/v2.rs static/openapi.v2.json tests/reference_fx.rs tests/reference_fx_sync.rs tests/v2_migrations.rs
git commit -m "feat(reference-data): add durable external fx observations"
~~~

---

## Task 10: Create Reporting schema and exactly-once projectors

**Files:**

- Create: src/infrastructure/migrations_v2/0008_reporting.sql
- Modify: src/contexts/reporting/{mod,public}.rs
- Create: src/contexts/reporting/application/{projectors,queries,ports,mod}.rs
- Create: src/contexts/reporting/infrastructure/{projections,queries,mod}.rs
- Create: tests/reporting_projections.rs
- Modify: src/contexts/mod.rs

- [ ] **Step 1 — RED: write schema/projector/rebuild tests**

Cover posting/reversal/correction/transfer semantics; asset/liability display signs; annotation; provider observation; the complete Ledger reconciliation lifecycle (`observed`, `matched`, `superseded`, `ignored-older`, `approved`, `dismissed`, `stale/refreshed`) with case/version/balance-version/observation-ordering semantics; recurring totals; Reference Data FX observation/revision/inversion/as-of consumption; exact base-currency conversion and explicit missing-rate states; exclusion of principal, control, transfer, and reconciliation-equity flows from income/expense; duplicate and out-of-order lifecycle events that never regress a reconciliation projection; dead letters/checkpoints; byte-equivalent truncate/replay; and ownership of initially empty bill-position, loan-summary, and Portfolio-valuation projection tables with no foreign keys into another context's schema.

- [ ] **Step 2: run RED**

~~~bash
SQLX_OFFLINE=true cargo test --test reporting_projections -- --nocapture
~~~

Expected: FAIL.

- [ ] **Step 3 — GREEN: add migration 0008 and projectors**

Projection change, consumed-event row, and checkpoint commit in one Reporting UoW. Register every frozen Ledger reconciliation lifecycle event in the central projector dispatch; select state by `(case_version, ledger_event_sequence, event_id)` and retain observation/decision history so late duplicate/older events cannot return an approved/dismissed/stale case to pending. Project versioned FX observations locally with their source IDs/effective times; historical reports select only a valid as-of rate and expose missing/incomplete conversion instead of falling back to a current rate. Create the report-owned bill-position, loan-summary, and Portfolio-valuation tables now so Phases 5–7 add event consumers rather than cross-context DDL. Store source-context identifiers as opaque values and add no foreign keys into Reference Data, Sharing, Loans, or Portfolio. Reporting exposes no financial command port.

- [ ] **Step 4: run focused tests**

~~~bash
SQLX_OFFLINE=true cargo test --test reporting_projections -- --nocapture
~~~

Expected: PASS.

- [ ] **Step 5 — REFACTOR: scan foreign SQL/imports**

~~~bash
rg -n "crate::contexts::(ledger|banking|recurring|reference_data)::(domain|application|infrastructure)" src/contexts/reporting
rg -n "(FROM|JOIN|UPDATE|INTO) (ledger|banking|recurring|reference_data)\\." src/contexts/reporting
~~~

Expected: no matches.

- [ ] **Step 6: commit**

~~~bash
git add src/infrastructure/migrations_v2/0008_reporting.sql src/contexts/reporting src/contexts/mod.rs tests/reporting_projections.rs
git commit -m "feat(reporting): add rebuildable v2 projections"
~~~

---

## Task 11: Add Reporting query API

**Files:**

- Create: src/contexts/reporting/api/{dto,handlers,routes,mod}.rs
- Create: tests/reporting_api.rs
- Modify: src/api/v2.rs
- Modify: src/bootstrap/v2.rs
- Modify: static/openapi.v2.json

- [ ] **Step 1 — RED: write report API tests**

Cover the exact frozen paths `GET /reports/balance-history`, `/reports/cashflow`, `/reports/spending`, `/reports/liabilities`, `/reports/reconciliations`, `/reports/recurring`, and `/reports/net-worth`; reject undeclared aliases. Cover date/timezone boundaries, source/base currency and missing rates, tenant isolation, projection sequence/as-of/lag, and the common response metadata contract.

- [ ] **Step 2: run RED**

~~~bash
cargo test --test reporting_api
~~~

Expected: FAIL.

- [ ] **Step 3 — GREEN: implement query routes and OpenAPI**

Implement exactly the Reporting route table frozen above and compose Reporting only in `src/api/v2.rs`. Unavailable Portfolio/Loan sections are empty/null until their owning events arrive. Never copy Ledger balance into a missing provider value.

- [ ] **Step 4: run focused tests**

~~~bash
cargo test --test reporting_api -- --nocapture
cargo test --test openapi_v2 -- --nocapture
~~~

Expected: PASS.

- [ ] **Step 5 — REFACTOR: keep query SQL explicit**

Share filter value objects, not a generic repository.

- [ ] **Step 6: commit**

~~~bash
git add src/contexts/reporting/api src/api/v2.rs src/bootstrap/v2.rs static/openapi.v2.json tests/reporting_api.rs
git commit -m "feat(api): add parallel reporting API"
~~~

---

## Task 12: Wire V2-only workers and prove restart/reversal workflow

**Files:**

- Create: tests/phase4_workflow.rs
- Modify: src/bootstrap/v2.rs
- Modify: src/integration/process_managers/mod.rs
- Modify: static/openapi.v2.json

- [ ] **Step 1 — RED: write full workflow test**

Prove Gmail reconnect, durable sync/evidence, Subscription/ChargeEvidence creation, local Ledger candidate, allocated match, idempotent annotation, unmatch rejected while categorization is unresolved, accepted unmatch with version-checked compensation after annotation, newer-user-annotation conflict visibility, NBU observation sync/replay/restart, preserved currency-catalog routes, exact and missing-rate Reporting conversion, Ledger reconciliation observed→approved plus out-of-order duplicate replay, Ledger reversal/unmatch, Reporting reversal, exact Reporting rebuild, and secret-free logs.

- [ ] **Step 2: run RED**

~~~bash
cargo test --test phase4_workflow -- --nocapture
~~~

Expected: FAIL.

- [ ] **Step 3 — GREEN: wire V2 bootstrap**

Register only in src/bootstrap/v2.rs. Use bounded generic workers; do not modify the default legacy runtime or DATABASE_URL.

- [ ] **Step 4: run Phase 4 suite**

~~~bash
cargo test --test mail_domain
cargo test --test mail_persistence
cargo test --test mail_sync
cargo test --test recurring_domain
cargo test --test recurring_matching
cargo test --test recurring_api
cargo test --test reference_fx
cargo test --test reference_fx_sync
cargo test --test reporting_projections
cargo test --test reporting_api
cargo test --test phase4_workflow
~~~

Expected: PASS.

- [ ] **Step 5 — REFACTOR: boundary/legacy scans**

~~~bash
cargo test --test context_boundaries
rg -n "src/infrastructure/migrations/" src/contexts/mail src/contexts/recurring src/contexts/reference_data src/contexts/reporting
~~~

Expected: no legacy migration use or context-boundary failures.

- [ ] **Step 6: commit**

~~~bash
git add src/bootstrap/v2.rs src/integration/process_managers/mod.rs static/openapi.v2.json tests/phase4_workflow.rs
git commit -m "feat(phase4): complete mail recurring reporting"
~~~

---

## Verification commands

~~~bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
SQLX_OFFLINE=true cargo test --all-targets -- --nocapture
cargo test --test openapi_v2 -- --nocapture
git diff --exit-code -- src/infrastructure/migrations tests/migrations.rs
cargo test --test migrations -- --nocapture
~~~

Expected: formatting, clippy, all targets, `static/openapi.v2.json` validation, the byte-for-byte legacy migration diff guard, and the frozen legacy checksum suite all PASS.

## Commit boundaries

1. Mail schema and constraints.
2. Immutable Mail aggregates/persistence.
3. Encrypted, restart-safe Gmail synchronization.
4. Characterized receipt parsers and immutable evidence events.
5. Recurring schema/domain.
6. Mail-evidence consumption and Recurring UoW.
7. Local Ledger projection, allocated matching, and categorization process.
8. Recurring API/OpenAPI.
9. Reference Data FX schema/sync/API.
10. Reporting schema and exactly-once projections.
11. Reporting API/OpenAPI.
12. Integrated restart/reversal/FX workflow.

Keep Mail, Recurring, and Reporting commits independently reviewable. Do not combine credentials, matching policy, and report arithmetic in one change.

## Exit criteria

- [x] Gmail credentials/verifiers are encrypted-only; secrets/raw bodies are absent from logs.
- [x] Mail exposes only the frozen connect/callback/list/status/disconnect/resync paths; existing-connection commands enforce `expected_version` and resync is a durable idempotent `202` command.
- [x] `mail.command_receipts` and `recurring.command_receipts` are context-owned and atomic with effects/outbox; same-key/same-hash calls replay exactly and same-key/different-hash calls return `409 idempotency_conflict` under concurrency and restart.
- [x] Messages and attempts are immutable/append-only with durable leases/cursors.
- [x] Existing Google Play, Apple, and Netflix fixture behavior passes in V2.
- [x] Recurring uses a local Ledger projection and allocated multi-journal matches.
- [x] Recurring and Reporting expose exactly the frozen paths and `static/openapi.v2.json` rejects undeclared aliases.
- [x] Matches/rejections/unmatches survive duplicates and restart.
- [x] Categorization goes only through Ledger public annotation.
- [x] External FX observations are immutable Decimal facts; NBU sync is leased/retry-safe and never logs raw bodies.
- [x] Reporting projects rates locally, uses historical as-of semantics, and exposes missing conversion instead of silently using a current rate.
- [x] Reporting covers balance, cashflow, spending, liabilities, reconciliation, recurring, and net worth.
- [x] Reporting rebuild is exact and has no financial write capability.
- [x] No Mail, Recurring, or Reporting SQL names a foreign context table.
- [x] Parallel OpenAPI validates; default runtime/API/DATABASE_URL remain legacy.
- [x] Blank-V2 migration, boundaries, format, clippy, and all Phase 4 tests pass.

## Out of scope

- Copying legacy Gmail, subscription, charge, or report rows.
- Automatic posting merely because a subscription is expected.
- Bill-sharing, loan, and Portfolio event consumers/business workflows; Phase 4 only pre-provisions their Reporting-owned projection storage.
- A generic cron engine.
- Automatic execution or repricing of a Ledger FX transfer from a reference rate; users/processes submit explicit confirmed source and target amounts.
- Mounting the V2 router or switching DATABASE_URL before Phase 8.
