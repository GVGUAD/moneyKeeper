# Finance V2 Phase 3 — Banking Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Keep checkbox state in this document and execute tasks in order.

**Goal:** Build a provider-neutral Banking context and its first Monobank adapter so one encrypted personal X-Token can discover cards/current accounts and jars, map eligible resources to Ledger accounts, import revised provider events exactly once financially, and surface provider balance discrepancies for explicit user reconciliation.

**Dependencies:** Finance V2 Phases 1 and 2 are complete. The isolated `v2_test_db`, shared kernel, transactional outbox/inbox, fenced process-manager runtime, and replacement router test harness exist. Ledger's provider-neutral account-validation, provider-import, provider-balance-observation, reconciliation-approval, idempotency, and event contracts are frozen. V2 migrations `0001`–`0003` pass on a blank database.

**Architecture:** `ProviderConnection`, `ExternalResource`, `ProviderEvent`, `SyncJob`, and `BalanceObservation` are Banking-owned aggregates or durable facts. Monobank is isolated behind an anti-corruption layer that translates provider JSON into provider-neutral Banking types. A connection owns one encrypted token and one encrypted high-entropy webhook credential plus a keyed lookup digest; Monobank resources represent individual cards/current accounts or jars (`банка`). Only cash-like resources may map to a Ledger account. The provider-neutral model reserves an unmappable `SecurityPortfolio` kind for a future provider, but the Monobank adapter does not fabricate securities/ОВДП resources; Phase 7 owns manual ОВДП. Provider events first enter a durable, revision-aware inbox; a shared durable process manager invokes Ledger's public import command with a derived idempotency key. Provider balance observations are persisted in Banking, then submitted to Ledger's provider-neutral `ObserveProviderBalance` command. Ledger owns the resulting `ReconciliationCase`; postings can change only through Ledger's later visible, version-checked approval command.

**Tech Stack:** Rust 2024, PostgreSQL 16, sqlx 0.8, axum 0.8, tokio, reqwest, rust_decimal, uuid, chrono, serde, aes-gcm, rand, sha2, thiserror, testcontainers

**Spec:** `docs/superpowers/specs/2026-08-05-finance-ddd-v2-design.md`

---

## Non-negotiable decisions

- This phase is parallel V2 construction only. Use `src/infrastructure/v2_test_db.rs` and the isolated V2 router in tests.
- Do **not** modify or wire `src/main.rs`, `src/api/routes.rs`, `src/infrastructure/db.rs`, `src/infrastructure/test_db.rs`, `tests/common/mod.rs`, `tests/migrations.rs`, Docker files/volumes, environment files, or `DATABASE_URL`. Phase 8 owns the irreversible reset and promotion.
- Do not edit `src/infrastructure/migrations/`. Banking is migration `src/infrastructure/migrations_v2/0004_banking.sql`; the V2 migration history is applied only to blank/already-marked V2 databases.
- Do not route or construct legacy `MonobankService`, `PgBankConnectionRepository`, `ReqwestMonobankClient`, legacy account/transaction repositories, or legacy Monobank handlers from V2 code.
- Banking never imports Ledger domain/application/infrastructure modules, queries `ledger.*`, or writes Ledger tables. It calls only provider-neutral contracts from `contexts::ledger::public`.
- One personal X-Token is one `ProviderConnection`. The token is encrypted before persistence with connection/user/provider-bound associated data. It is never dual-written, returned by an API, formatted by `Debug`, or logged.
- Reauthorization replaces the encrypted credential **within the same connection identity** through a version-fenced task; it never creates a second connection merely to preserve resources/mappings. A candidate credential is validated before activation, old worker generations are fenced, and credential-version/audit history contains no plaintext.
- An `ExternalResource` is one provider resource. Cards/current accounts and jars are distinct resources. An optional mapping stores an opaque Ledger account ID, but does not transfer aggregate ownership to Banking.
- Resource mappings are versioned/audited facts, never destructive links. A user may deactivate a mistaken mapping or replace it after validation; the old mapping and its effective revision/time remain visible. Replacement affects only subsequently imported provider revisions and never silently moves prior Ledger journals.
- Only `card`, `current_account`, and `jar` resources may map to compatible Ledger Asset/Liability accounts with the same user and currency. Provider-neutral `security_portfolio` resources cannot map to a scalar Ledger account and would publish a versioned discovery event for Portfolio. The Phase 3 Monobank adapter discovers/tests cards/current accounts and jars only; unknown Monobank products are quarantined, never guessed to be securities/ОВДП.
- Provider events are durable facts. Their identity is `(connection_id, external_resource_id, external_event_id, revision)`. A later pending/settled state, amount correction, or reversal is a retained revision, never discarded by `ON CONFLICT DO NOTHING` on the external event ID.
- The first monetarily complete pending/hold revision posts one provisional-source Ledger journal and affects the calculated Ledger balance. A settled revision with identical money appends provider/link state only and creates no second postings; a monetary correction reverses/replaces, and a provider cancellation/reversal posts an explicit reversal. Pending is never a mutable Ledger journal status.
- Import is at-least-once across the context boundary but has at most one financial effect. The Banking process manager derives a stable Ledger `IdempotencyKey` from provider, resource, event, and revision.
- A sync page/window cursor advances only after every event in that page is durably `processed` or explicitly `quarantined`. A transient Ledger/provider failure keeps the cursor fixed and remains retryable after restart.
- Provider-reported, available, credit-limit, and statement-running balances are persisted as Banking observations, then passed to Ledger's provider-neutral `ObserveProviderBalance` command. Banking stores only its observation plus delivery/link status; Ledger owns the case and approval/dismissal decisions. Observations never call `set_balance`, mutate a Ledger projection, or silently create a correction.
- Every balance observation records provider basis/sign semantics separately from any normalized Ledger-comparable display balance. The adapter may mark an observation `NotComparable(reason)`; it remains visible but creates no fake difference/case. Only a tested normalization compatible with the mapped account nature/currency is delivered for reconciliation.
- Webhook receipt authenticates an unguessable per-connection credential, handles Monobank validation, persists/queues work, and returns quickly. It never calls Ledger inline. Raw request/provider bodies are encrypted or discarded after normalization and are excluded from normal logs and error text.
- Disconnect is an audited state transition. It disables sync/webhooks and removes usable credential material; it does not delete imported history, events, observations, mappings, or Ledger reconciliation history.
- Route paths are unversioned inside the parallel V2 router because Phase 8 promotes that router as the replacement API. Do not add these paths to the legacy public router.

## Entry gate

- [ ] `cargo test --test v2_migrations` passes through migration `0003` on a blank V2 database.
- [ ] `cargo test --test integration_runtime` proves transactional outbox/inbox, retries, and fencing.
- [ ] `cargo test --test ledger_public_contracts` freezes provider resource validation, import revision, reversal/replacement, provider-balance observation, and reconciliation approval outcomes.
- [ ] `cargo test --test context_boundaries` prevents Banking from importing Ledger internals or querying `ledger.*`.
- [ ] The isolated V2 router/test harness can be constructed without starting the legacy runtime or background workers.

Run:

~~~bash
SQLX_OFFLINE=true cargo test --test v2_migrations -- --nocapture
cargo test --test integration_runtime -- --nocapture
cargo test --test ledger_public_contracts -- --nocapture
cargo test --test context_boundaries -- --nocapture
~~~

Expected: PASS. If a Ledger public contract is missing, add it to the Phase 2 implementation before beginning Banking; do not bypass the boundary with a repository import.

---

## File map

| File | Action | Responsibility |
|---|---|---|
| `src/infrastructure/migrations_v2/0004_banking.sql` | Create | Banking connections/webhook registration state, encrypted credentials, resources/mappings, revisioned events, observations/delivery links, sync jobs/pages, constraints, and worker indexes |
| `src/contexts/banking/mod.rs` | Modify | Keep layers private and export only `api` and `public` |
| `src/contexts/banking/public.rs` | Modify | Replace the Phase 1 skeleton with Banking commands/read models and versioned events for Reporting/Portfolio |
| `src/contexts/banking/domain/mod.rs` | Create | Domain exports |
| `src/contexts/banking/domain/ids.rs` | Create | Banking-owned connection/resource/event/job/observation IDs |
| `src/contexts/banking/domain/connection.rs` | Create | `ProviderConnection`, state, version, webhook rotation |
| `src/contexts/banking/domain/resource.rs` | Create | `ExternalResource`, product kind, discovery state, Ledger mapping policy |
| `src/contexts/banking/domain/provider_event.rs` | Create | Provider event identity, revisions, normalized financial state |
| `src/contexts/banking/domain/sync_job.rs` | Create | Requested range, page cursor, retry state, fenced execution |
| `src/contexts/banking/domain/balance_observation.rs` | Create | Immutable provider balance facts and delivery/link status to Ledger-owned cases |
| `src/contexts/banking/domain/error.rs` | Create | Stable validation/conflict/terminal/transient errors |
| `src/contexts/banking/application/mod.rs` | Create | Application exports |
| `src/contexts/banking/application/commands.rs` | Create | Connect/credential-replace/disconnect, discover, map, sync, webhook registration/rotation, and observation command DTOs |
| `src/contexts/banking/application/handlers.rs` | Create | Banking aggregate/UoW orchestration |
| `src/contexts/banking/application/queries.rs` | Create | Tenant-scoped connection/resource/event/job/process/observation reads |
| `src/contexts/banking/application/ports.rs` | Create | Banking UoW/repository, cipher, provider, Ledger public port, clock, outbox ports |
| `src/contexts/banking/application/sync.rs` | Create | Durable page-fetch/process/cursor state machine |
| `src/contexts/banking/infrastructure/mod.rs` | Create | PostgreSQL and Monobank adapter exports |
| `src/contexts/banking/infrastructure/rows.rs` | Create | Private SQL row mappings |
| `src/contexts/banking/infrastructure/pg_unit_of_work.rs` | Create | Transaction-bound Banking repositories/outbox/inbox writes |
| `src/contexts/banking/infrastructure/pg_repositories.rs` | Create | Aggregate/fact persistence and tenant-scoped queries |
| `src/contexts/banking/infrastructure/credential_cipher.rs` | Create | AES-256-GCM encrypted-only token/raw-provenance adapter and key rotation |
| `src/contexts/banking/infrastructure/webhook_secret.rs` | Create | CSPRNG credential generation, encrypted recovery for registration, keyed lookup digest, constant-time validation |
| `src/contexts/banking/infrastructure/monobank/mod.rs` | Create | Monobank adapter exports |
| `src/contexts/banking/infrastructure/monobank/client.rs` | Create | Rate-aware HTTP client with redacted error handling |
| `src/contexts/banking/infrastructure/monobank/dto.rs` | Create | Provider wire DTOs only |
| `src/contexts/banking/infrastructure/monobank/normalizer.rs` | Create | Anti-corruption mapping for cards/current accounts, jars, events, revisions, observations, and unknown-product quarantine |
| `src/contexts/banking/api/mod.rs` | Create | Banking API exports |
| `src/contexts/banking/api/dto.rs` | Create | Decimal-string, redacted HTTP DTOs |
| `src/contexts/banking/api/handlers.rs` | Create | Auth-scoped parallel V2 command/query handlers |
| `src/contexts/banking/api/routes.rs` | Create | Connection/resource/mapping/sync/observation and webhook routes |
| `src/integration/process_managers/mod.rs` | Modify Phase 1 parent | Export durable process-manager registrations |
| `src/integration/process_managers/banking_resource_mapping.rs` | Create | Create-or-bind provider resource to Ledger account without cross-context transaction |
| `src/integration/process_managers/banking_import.rs` | Create | Provider-event-to-Ledger import coordinator |
| `src/integration/process_managers/banking_observation.rs` | Create | Banking-observation-to-Ledger case delivery coordinator |
| `src/contexts/mod.rs` | Modify | Export the Banking public/API entry points |
| `src/api/v2.rs` | Modify | Compose Banking routes only into the isolated V2 router |
| `src/api/v2_state.rs` | Modify | Add Ledger+Banking account read composition through public façades only |
| `src/bootstrap/v2.rs` | Modify | Construct Banking adapters and bounded worker handles for isolated tests/future promotion |
| `static/openapi.v2.json` | Modify | Add the future replacement Banking contract; never expose secrets/raw payloads |
| `tests/banking_domain.rs` | Create | Aggregate, resource-kind, revision, mapping, and observation tests |
| `tests/banking_persistence.rs` | Create | Migration constraints, encryption, immutability, tenancy, claims, and cursor tests |
| `tests/banking_monobank.rs` | Create | Wire DTO/normalizer/client, redaction, rate-limit, and provider revision tests |
| `tests/banking_sync.rs` | Create | Duplicate/revision/crash/restart/partial-page/concurrency workflow tests |
| `tests/banking_webhook.rs` | Create | Secret validation, handshake, fast durable intake, and log-redaction tests |
| `tests/banking_api.rs` | Create | Isolated V2 API/auth/idempotency/status tests |
| `tests/banking_ledger_contract.rs` | Create | Provider-neutral fake/real Ledger public-port integration tests |
| `tests/phase3_workflow.rs` | Create | End-to-end connect/discover/map/sync/reconcile/restart scenario |

Use the exact Phase 1/2 names if those phases landed a helper under a slightly different path. Rename this plan reference during implementation rather than create a second V2 router, migrator, integration runtime, credential keyring, or Ledger port.

---

## Task 1: Model Banking aggregates and provider-neutral facts

**Files:**

- Create: `src/contexts/banking/domain/{mod.rs,ids.rs,connection.rs,resource.rs,provider_event.rs,sync_job.rs,balance_observation.rs,error.rs}`
- Modify: `src/contexts/banking/public.rs`
- Modify: `src/contexts/banking/mod.rs`
- Create: `tests/banking_domain.rs`

- [ ] **Step 1 — RED: specify connection and resource invariants**

Add pure tests covering:

~~~text
one active personal token per ProviderConnection plus at most one encrypted pending replacement candidate
connection states pending, active, pending_credential_validation, needs_reauth, revoked
all connection mutations require expected version
token and webhook credentials are redacted from Debug/Serialize
distinct card/current-account and jar resources under one connection
stable provider resource identity scoped to connection
resource currency is immutable after mapping/import
only cash-like kinds are Ledger-mappable
provider-neutral security_portfolio mapping is rejected with RouteToPortfolio
mapping cannot silently change; unmap/remap is versioned and audited
disconnect revokes work but retains non-secret history
~~~

- [ ] **Step 2 — RED: specify event, sync, and observation state machines**

Test identity `(connection, resource, external_event_id, revision)`, revision ordering, pending-to-settled transitions, non-monetary versus monetary revisions, explicit provider reversal, valid processing states, page completion rules, fenced job execution, retry classification, immutable observation facts, provider balance basis/sign, comparable normalization versus visible `NotComparable`, and observation delivery/link states. Reconciliation-case lifecycle stays in Ledger tests because Ledger owns that aggregate.

- [ ] **Step 3: run and capture RED**

~~~bash
cargo test --test banking_domain -- --nocapture
~~~

Expected: FAIL because the Banking domain does not exist.

- [ ] **Step 4 — GREEN: implement the smallest provider-neutral model**

Use universal IDs/`Clock`/`Money`/`CurrencyCode` from the shared kernel and Banking-owned ID newtypes built with the shared macro, plus opaque credential handles and explicit versions. Do not include `Mono*` names in domain/application types. Model `ResourceKind::{Card, CurrentAccount, Jar, SecurityPortfolio, Unsupported}`, `FundingModel::{OwnFunds, RevolvingCredit, Unknown}`, and event `ProviderTransactionState::{Pending, Settled, Reversed}`. A normalized revision records effective/recorded times, original/operation money, merchant description/MCC, status, content digest, and optional provider running-balance observation. Monobank maps a positively configured credit limit to `RevolvingCredit` and a confirmed zero-credit own-funds product to `OwnFunds`; absent or contradictory product metadata is `Unknown` and blocks automatic account creation/mapping. A later credit-policy change never mutates Ledger account nature silently—it moves the resource mapping to visible `NeedsReview`.

- [ ] **Step 5 — REFACTOR: enforce secret and context boundaries**

~~~bash
rg -n "(token|secret).*derive.*(Debug|Serialize)|crate::contexts::ledger::(domain|application|infrastructure)|sqlx::|reqwest::|Mono(bank)?" src/contexts/banking/domain src/contexts/banking/application
~~~

Expected: no credential serialization, Ledger-internal imports, SQLx/Reqwest dependencies, or Monobank wire concepts in provider-neutral layers.

- [ ] **Step 6: commit**

~~~bash
git add src/contexts/banking/domain src/contexts/banking/public.rs src/contexts/banking/mod.rs tests/banking_domain.rs
git commit -m "feat(banking): model provider connections and durable facts"
~~~

---

## Task 2: Create the strict Banking schema

**Files:**

- Create: `src/infrastructure/migrations_v2/0004_banking.sql`
- Create: `tests/banking_persistence.rs`
- Modify: `tests/v2_migrations.rs`

- [ ] **Step 1 — RED: write fresh-V2 schema tests**

Using only `v2_test_db`, test that migration `0004` creates:

~~~text
banking.provider_connections
banking.external_resources
banking.resource_mappings
banking.provider_events
banking.balance_observations
banking.sync_jobs
banking.sync_pages
banking.command_receipts
~~~

Prove composite tenant uniqueness/FKs, bounded provider IDs/errors, `TIMESTAMPTZ`, immediately valid state checks, encrypted-only credential columns, immutable event revisions/observations, durable webhook registration attempts, and indexes for connection/resource lookup, ready events, due jobs, leases, undelivered observations, and chronological inspection. `banking.command_receipts` is tenant/scoped-key unique and stores a canonical request hash plus the exact typed result/status so it can commit in the same Banking UoW as command state and outbox.

- [ ] **Step 2: run and capture RED**

~~~bash
SQLX_OFFLINE=true cargo test --test banking_persistence schema_ -- --nocapture
cargo test --test v2_migrations banking_migration -- --nocapture
~~~

Expected: FAIL because `0004_banking.sql` and its tables do not exist.

- [ ] **Step 3 — GREEN: add migration `0004_banking.sql`**

Use `UNIQUE(id, user_id)` and tenant-safe composite FKs within `banking`. Do not create a foreign key to `ledger.*` or `portfolio.*`; `resource_mappings.ledger_account_id` is an opaque typed UUID validated through Ledger's public facade. Provider event uniqueness is exactly `(connection_id, external_resource_id, external_event_id, revision)`, with an additional content digest to detect an unnumbered provider correction. Never make `external_event_id` alone unique.

Store token/provenance ciphertext, nonce/envelope version/key ID as an authenticated envelope or one versioned envelope column. Store the webhook credential in a separate authenticated encrypted envelope for retryable provider registration and also store a keyed lookup digest for request routing/constant-time comparison; never store its plaintext or full callback URL. Persist webhook desired/registered credential version, provider-registration state, attempts, next retry, and bounded error without the secret URL itself. Add append/update/delete guards so durable provider facts cannot be rewritten or hard-deleted. Mutable aggregate/status/link tables remain version checked.

- [ ] **Step 4: run focused schema tests**

~~~bash
SQLX_OFFLINE=true cargo test --test banking_persistence schema_ -- --nocapture
cargo test --test v2_migrations -- --nocapture
~~~

Expected: PASS.

- [ ] **Step 5 — REFACTOR: inspect ownership and unsafe SQL**

~~~bash
rg -n "REFERENCES (ledger|portfolio|public)\.|FLOAT|DOUBLE|ON CONFLICT.*DO NOTHING|token_plain|raw_payload JSON" src/infrastructure/migrations_v2/0004_banking.sql
~~~

Expected: no matches. Normal indexes are created inside this blank-database migration; do not use `CONCURRENTLY`, `NOT VALID`, compatibility views, or backfill SQL.

- [ ] **Step 6: commit**

~~~bash
git add src/infrastructure/migrations_v2/0004_banking.sql tests/banking_persistence.rs tests/v2_migrations.rs
git commit -m "feat(db): add strict v2 banking schema"
~~~

---

## Task 3: Persist encrypted connections and discover resources through the Monobank ACL

**Files:**

- Create: `src/contexts/banking/application/{mod.rs,commands.rs,handlers.rs,queries.rs,ports.rs}`
- Create: `src/contexts/banking/infrastructure/{mod.rs,rows.rs,pg_unit_of_work.rs,pg_repositories.rs,credential_cipher.rs}`
- Create: `src/contexts/banking/infrastructure/monobank/{mod.rs,client.rs,dto.rs,normalizer.rs}`
- Create: `tests/banking_monobank.rs`
- Modify: `tests/banking_persistence.rs`

- [ ] **Step 1 — RED: test encrypted connection persistence**

Test that a connection round-trip decrypts only through the cipher port; database text/byte scans do not contain the X-Token; ciphertext is bound by associated data to user/connection/provider; wrong key/AAD fails closed; key rotation re-encrypts without changing connection identity; API/read DTOs and debug output omit the token. There is no plaintext compatibility column or fallback mode.

- [ ] **Step 2 — RED: test provider discovery and normalization**

With a local fake HTTP server and sanitized fixtures, cover initial and replacement X-Token validation via client-info, card/current-account discovery, separate jar (`банка`) discovery, ISO numeric-code conversion through Reference Data, masked card/IBAN metadata, credit limits, reported/available balances, unknown-product quarantine, and timeout/429/5xx classification. Prove a valid replacement preserves connection/resource/mapping IDs, increments credential generation, re-runs discovery, fences old sync claims/cursor writes, and cryptographically retires the old credential; an invalid replacement leaves an active old generation active or a `NeedsReauth` connection unauthenticated without exposing either token. The Monobank fixtures do not invent or classify securities/ОВДП; `SecurityPortfolio` remains only a provider-neutral future kind.

Assert response bodies, X-Tokens, webhook credentials, card identifiers, and raw financial JSON are absent from captured logs and bounded errors.

- [ ] **Step 3: run and capture RED**

~~~bash
cargo test --test banking_monobank -- --nocapture
cargo test --test banking_persistence credential -- --nocapture
~~~

Expected: FAIL because the application and adapters do not exist.

- [ ] **Step 4 — GREEN: implement connect and discovery**

`ConnectProvider` encrypts the token before the Banking UoW commits a durable `PendingValidation` connection and validation-requested outbox event. `ReplaceProviderCredential` stores a separately encrypted candidate under a new credential generation and `PendingCredentialValidation` without changing connection/resource/mapping identity. A worker decrypts only the selected generation for the request duration and calls Monobank client-info. A valid candidate atomically activates its generation, fences old claims, retires old usable ciphertext, and schedules rediscovery/webhook registration; an invalid candidate is crypto-shredded and leaves the prior active generation unchanged when one exists, otherwise the connection remains visible `NeedsReauth`. Ordinary sync cannot use a pending/invalid generation. Discovery normalizes cards/current accounts and jars, upserts mutable resource metadata by stable external identity, and appends balance observations rather than replacing Ledger balances. A future provider adapter may publish `banking.security-resource-discovered.v1`; the Monobank adapter does not fabricate that resource.

- [ ] **Step 5 — REFACTOR: separate provider wire details**

Keep `X-Token`, Monobank numeric currency/product strings, minor-unit conversion, rate-limit headers, and JSON DTOs under `infrastructure/monobank`. The provider client logs only endpoint class, HTTP status, request ID, elapsed time, and redacted connection/resource IDs. Zeroize plaintext credential buffers when the existing secret type permits it.

- [ ] **Step 6: commit**

~~~bash
git add src/contexts/banking/application src/contexts/banking/infrastructure tests/banking_monobank.rs tests/banking_persistence.rs
git commit -m "feat(banking): encrypt connections and discover Monobank resources"
~~~

---

## Task 4: Validate resource mappings through Ledger's public facade

**Files:**

- Modify: `src/contexts/banking/application/{commands.rs,handlers.rs,ports.rs,queries.rs}`
- Modify: `src/contexts/banking/infrastructure/{pg_unit_of_work.rs,pg_repositories.rs}`
- Modify: `src/contexts/banking/public.rs`
- Create: `src/integration/process_managers/banking_resource_mapping.rs`
- Modify: `src/integration/process_managers/mod.rs`
- Modify: `tests/banking_domain.rs`
- Create: `tests/banking_ledger_contract.rs`

- [ ] **Step 1 — RED: write mapping contract tests**

Cover both mapping targets: bind an existing Ledger account, or request creation of a provider-observed Ledger account from the resource. Prove mapping succeeds only when the authenticated user owns the objects, resource/account currencies match, Ledger account authority/kind/nature accepts provider mapping, and neither side is archived. Validate the resource's native currency before either path. `OwnFunds` card/current resources require an Asset/debit-card-or-current target; `RevolvingCredit` card resources require a Liability/credit-card target; `Unknown` blocks create/map pending user-visible review. Reject cross-user IDs without revealing existence, wrong currency, incompatible funding model/nature, security/ОВДП, system accounts, duplicate active mappings, stale expected versions, and two resources mapped to one Ledger account unless the frozen Ledger contract explicitly permits it.

For create-and-map, prove the Banking command first persists `PendingAccountCreation` plus outbox/process correlation, a durable process manager calls Ledger's provider-neutral account-opening command with the exact kind/nature dictated by the confirmed funding model and key `banking-resource-account:{resource_id}:{mapping_version}`, and crash/retry creates one account and one active mapping. Also prove version-checked deactivation and replacement retain the old mapping/effective boundary, reject stale versions, and route only later provider revisions to the replacement account. A resource moved to `NeedsReview` by a funding-model change stops new imports until deactivation/replacement resolves it; prior Ledger journals are never rewritten. Banking must not hold a transaction open across the Ledger call.

- [ ] **Step 2: run and capture RED**

~~~bash
cargo test --test banking_ledger_contract mapping_ -- --nocapture
~~~

Expected: FAIL because Banking has no Ledger public-port adapter or mapping handler.

- [ ] **Step 3 — GREEN: implement public-facade validation and audited mapping**

For an existing account, call a provider-neutral Ledger query such as `validate_provider_account_binding(user_id, account_id, currency, expected_authority)`, then recheck Banking version and commit the audited mapping. For create-and-map, persist intent and let the shared process manager call Ledger, then commit the returned account ID only if the resource/mapping version still matches. Both paths publish `banking.resource-mapped.v1`; failures expose retryable or terminal status. A race is resolved by database uniqueness/version checks. Do not hold a Banking SQL transaction open while calling Ledger.

- [ ] **Step 4: run focused tests**

~~~bash
cargo test --test banking_ledger_contract mapping_ -- --nocapture
cargo test --test banking_persistence mapping_ -- --nocapture
~~~

Expected: PASS.

- [ ] **Step 5 — REFACTOR: prove there is no table coupling**

~~~bash
rg -n "ledger\.|Pg.*Ledger|contexts::ledger::(domain|application|infrastructure)" src/contexts/banking
~~~

Expected: the only Ledger reference is the provider-neutral `contexts::ledger::public` contract; no SQL table/repository/internal-layer reference appears.

- [ ] **Step 6: commit**

~~~bash
git add src/contexts/banking src/integration/process_managers/banking_resource_mapping.rs src/integration/process_managers/mod.rs tests/banking_domain.rs tests/banking_ledger_contract.rs tests/banking_persistence.rs
git commit -m "feat(banking): validate and audit ledger resource mappings"
~~~

---

## Task 5: Build the durable revision-aware provider event inbox

**Files:**

- Modify: `src/contexts/banking/application/{commands.rs,handlers.rs,ports.rs}`
- Modify: `src/contexts/banking/infrastructure/{pg_unit_of_work.rs,pg_repositories.rs}`
- Modify: `src/contexts/banking/infrastructure/monobank/normalizer.rs`
- Modify: `tests/banking_monobank.rs`
- Modify: `tests/banking_persistence.rs`
- Create: `tests/banking_sync.rs`

- [ ] **Step 1 — RED: specify duplicate and revision behavior**

Test:

~~~text
same resource/event/revision/content delivered twice -> one ProviderEvent revision
same external event ID on two resources -> two independent facts
pending then settled with unchanged money -> two visible provider revisions, one monetary intent
settled revision with changed amount/currency -> retained correction revision
provider reversal -> retained reversal revision
same claimed revision with different content digest -> conflict/quarantine, never silent overwrite
events for unknown/unmapped resources -> retained pending/quarantined status, not dropped
malformed or unsupported currency/product -> bounded quarantine reason and encrypted provenance
~~~

- [ ] **Step 2: run and capture RED**

~~~bash
cargo test --test banking_sync -- --nocapture
~~~

Expected: FAIL because durable intake is absent.

- [ ] **Step 3 — GREEN: implement transactional event intake**

Normalize before domain construction, calculate a canonical SHA-256 content digest, encrypt permitted raw provenance, and insert/upsert by the full revision identity. Same identity+digest returns the existing receipt. Same identity+different digest is recorded as a conflict/quarantine requiring inspection. A new revision commits the provider fact, balance observation when present, audit record, and `banking.provider-event-ready.v1` outbox message atomically.

Do not use `ON CONFLICT DO NOTHING` as the business decision. Use an explicit `INSERT ... ON CONFLICT ... RETURNING`/locked read that distinguishes duplicate, new revision, and conflicting content.

- [ ] **Step 4: run focused tests**

~~~bash
cargo test --test banking_sync -- --nocapture
cargo test --test banking_persistence event_ -- --nocapture
~~~

Expected: PASS.

- [ ] **Step 5 — REFACTOR: make provenance retention explicit**

Document/test the retention class of encrypted raw provider payloads and ensure normal query DTOs return only normalized facts, digest, provider IDs in redacted form, state, attempts, and error class. No API returns ciphertext or raw payload.

- [ ] **Step 6: commit**

~~~bash
git add src/contexts/banking tests/banking_monobank.rs tests/banking_persistence.rs tests/banking_sync.rs
git commit -m "feat(banking): persist revision-aware provider events"
~~~

---

## Task 6: Implement restart-safe sync jobs, leases, and cursor discipline

**Files:**

- Create: `src/contexts/banking/application/sync.rs`
- Modify: `src/contexts/banking/application/{commands.rs,handlers.rs,ports.rs,queries.rs}`
- Modify: `src/contexts/banking/infrastructure/{pg_unit_of_work.rs,pg_repositories.rs}`
- Modify: `src/contexts/banking/infrastructure/monobank/client.rs`
- Modify: `tests/banking_sync.rs`

- [ ] **Step 1 — RED: write crash, retry, and concurrency tests**

Using fake time and a scripted provider, prove:

~~~text
requested range and overlap window survive process restart
one credential has at most one active provider request/rate-limit lease
two workers cannot own the same sync job/page concurrently
stale fencing token cannot update a job after lease reacquisition
429 honors Retry-After and does not consume/skip a page
timeout/5xx back off with bounded attempts and redacted errors
crash after page fetch/intake resumes without duplicate event facts
crash after some Ledger imports leaves the page cursor unchanged
cursor advances exactly once only when all page events are processed or explicitly quarantined
terminal quarantine is visible and requires an explicit reason; transient failure is never auto-quarantined
disconnect/credential rotation fences an old worker
~~~

- [ ] **Step 2: run and capture RED**

~~~bash
cargo test --test banking_sync -- --nocapture
~~~

Expected: FAIL because the sync executor does not exist.

- [ ] **Step 3 — GREEN: implement the durable sync state machine**

Use the Phase 1 shared lease/process primitives rather than a detached `tokio::spawn` per connection. Persist requested range, current page/window, overlap, attempt, next retry, last bounded error, connection version, and fencing token. The sequence is:

1. claim the due job and per-connection/provider-rate lease;
2. fetch one bounded Monobank statement window without a database transaction;
3. transactionally persist the page and all normalized revision receipts;
4. allow the import process manager to finish every event;
5. transactionally mark the page complete and advance the durable cursor; and
6. release/renew with the current fencing token.

- [ ] **Step 4: run focused tests**

~~~bash
cargo test --test banking_sync -- --nocapture
cargo test --test integration_runtime lease -- --nocapture
~~~

Expected: PASS.

- [ ] **Step 5 — REFACTOR: bound work and make restart behavior inspectable**

Use configurable batch/window sizes and jittered backoff. Do not sleep while holding a PostgreSQL connection or lease transaction. Every job/page exposes state, cursor, attempts, retry time, last error class, and blocking event counts to read queries.

- [ ] **Step 6: commit**

~~~bash
git add src/contexts/banking/application/sync.rs src/contexts/banking/application src/contexts/banking/infrastructure tests/banking_sync.rs
git commit -m "feat(banking): add durable fenced Monobank sync"
~~~

---

## Task 7: Import provider revisions through the shared process manager and Ledger public command

**Files:**

- Create: `src/integration/process_managers/banking_import.rs`
- Modify: `src/integration/process_managers/mod.rs`
- Modify: `src/contexts/banking/application/{ports.rs,handlers.rs,queries.rs}`
- Modify: `src/contexts/banking/infrastructure/{pg_unit_of_work.rs,pg_repositories.rs}`
- Modify: `src/contexts/banking/public.rs`
- Modify: `tests/banking_ledger_contract.rs`
- Modify: `tests/banking_sync.rs`

- [ ] **Step 1 — RED: write end-to-end idempotency/revision tests**

With a real isolated Ledger facade where practical, prove:

~~~text
duplicate provider-ready delivery -> one Ledger financial effect
crash after Ledger commit before Banking acknowledgment -> retry returns same Ledger result
pending-to-settled without monetary change -> provider state changes, postings do not
amount/currency correction -> explicit Ledger reversal/replacement chain
provider reversal -> explicit Ledger reversal, never DELETE/UPDATE of posted monetary rows
unmapped resource -> visible waiting state and no Ledger call
business rejection -> terminal visible failure; transient error -> retry
concurrent workers -> one completed import outcome per revision
revision N+1 claimed before N completes -> visible WaitingForPriorRevision and no premature Ledger reverse/replace
predecessor crash/retry -> successors resume in causal revision order without duplicate effects
~~~

- [ ] **Step 2: run and capture RED**

~~~bash
cargo test --test banking_ledger_contract -- --nocapture
cargo test --test banking_sync -- --nocapture
~~~

Expected: FAIL because the Banking import process manager is absent.

- [ ] **Step 3 — GREEN: implement the durable import coordinator**

Consume `banking.provider-event-ready.v1` through the shared inbox and process store. Translate the normalized event to Ledger's provider-neutral `ImportProviderTransaction` command. Derive the key from a canonical form such as `banking-import:{provider}:{connection}:{resource}:{event}:{revision}` and pass source/effective/recorded times, normalized money, state, mapping, and prior revision identity. Serialize by `(connection, resource, external_event_id)` and require every lower known revision to be terminal `Posted`, `NoFinancialChange`, or an explicitly quarantined terminal rejection before invoking Ledger for the next revision. An out-of-order successor persists `WaitingForPriorRevision`; predecessor completion emits/wakes the successor. Never pass a raw Monobank DTO or token.

Persist `WaitingForMapping`, `WaitingForPriorRevision`, `Posting`, `Posted`, `NoFinancialChange`, `TerminalFailure`, or `RetryDue` process state. If Ledger already committed, its idempotency receipt returns the prior `JournalEntry`/reversal/replacement result and Banking safely completes. Commit process completion and Banking event status through the Banking UoW/inbox boundary. A higher revision cannot treat a merely claimed/failed/retrying predecessor as financially complete.

- [ ] **Step 4: run focused integration tests**

~~~bash
cargo test --test banking_ledger_contract -- --nocapture
cargo test --test banking_sync -- --nocapture
~~~

Expected: PASS and Ledger projection rebuild remains equal to posting sums.

- [ ] **Step 5 — REFACTOR: freeze the cross-context contract**

Add contract snapshots for the public command/outcome and `banking.provider-event-ready.v1`. Consumers ignore additive fields but reject unknown major versions. Verify the process-manager state contains IDs and minimum facts only, not tokens or raw provider payloads.

- [ ] **Step 6: commit**

~~~bash
git add src/integration/process_managers src/contexts/banking tests/banking_ledger_contract.rs tests/banking_sync.rs
git commit -m "feat(banking): import provider revisions idempotently"
~~~

---

## Task 8: Deliver stored balance observations to Ledger-owned reconciliation

**Files:**

- Create: `src/integration/process_managers/banking_observation.rs`
- Modify: `src/integration/process_managers/mod.rs`
- Modify: `src/contexts/banking/domain/balance_observation.rs`
- Modify: `src/contexts/banking/application/{commands.rs,handlers.rs,ports.rs,queries.rs}`
- Modify: `src/contexts/banking/infrastructure/{pg_unit_of_work.rs,pg_repositories.rs}`
- Modify: `src/contexts/banking/public.rs`
- Modify: `tests/banking_domain.rs`
- Modify: `tests/banking_ledger_contract.rs`

- [ ] **Step 1 — RED: prove observations never overwrite Ledger**

Test reported balance, available balance, credit limit, statement-running balance, basis/sign semantics, optional normalized Ledger-comparable balance, currency, provider observation time, recorded time, stable per-resource source sequence, resource, and provenance. Banking must durably store the observation before invoking Ledger's provider-neutral `ObserveProviderBalance` command. Deliver newer then older observations and deliver both concurrently; after either step, Ledger balance/posting counts must be unchanged and the active case must never regress. Ledger creates/refreshes only from the greatest `(observed_at, source_sequence, observation_id)` and returns `IgnoredOlderObservation` linked to the active case for an older fact; `NotComparable` remains visible with no Ledger call/case. Banking retains the provider-specific facts and only the delivery/link result.

- [ ] **Step 2 — RED: prove Ledger retains approval authority**

Through Ledger's frozen public/API contract, test that approval requires actor, reason, expected case version, and the captured Ledger balance version. If Ledger changed, the outcome is `Stale`; it never applies the old delta. Approval produces a visible Ledger reconciliation `JournalEntry`, duplicate approval returns the same result, and dismissal is non-financial. Banking cannot approve/dismiss and stores no second reconciliation aggregate; it may consume Ledger decision events for a local read status.

- [ ] **Step 3: run and capture RED**

~~~bash
cargo test --test banking_ledger_contract -- --nocapture
cargo test --test banking_domain reconciliation -- --nocapture
~~~

Expected: FAIL because Banking observation delivery is incomplete.

- [ ] **Step 4 — GREEN: implement durable observation delivery**

Persist the Banking observation first and assign its monotonic per-resource source sequence. A `NotComparable` observation becomes terminal/visible without invoking Ledger. Otherwise use the shared process manager to invoke Ledger's provider-neutral `ObserveProviderBalance` command with the normalized comparable Money, stable observation idempotency key, minimal source-stream reference, and full ordering tuple. Persist `Delivered { reconciliation_case_id, status }`, `IgnoredOlder { active_case_id }`, `RetryDue`, or terminal rejection without copying Ledger's case. Ledger calculates the comparison and owns latest-stream serialization plus later approve/dismiss/stale behavior. Publish `banking.balance-observed.v1` with provider-specific facts/basis/comparability for Reporting; optionally consume Ledger decision events only to update Banking's read link/status.

- [ ] **Step 5 — REFACTOR: search for forbidden balance mutation**

~~~bash
rg -n "set_balance|adjust_balance|UPDATE ledger\.|account_balance_projection|balance\s*=" src/contexts/banking src/integration/process_managers/banking_observation.rs
~~~

Expected: no direct Ledger balance mutation. Assignments to Banking observation/read DTO fields are acceptable; inspect any match manually.

- [ ] **Step 6: commit**

~~~bash
git add src/contexts/banking src/integration/process_managers tests/banking_domain.rs tests/banking_ledger_contract.rs
git commit -m "feat(banking): deliver provider balance observations"
~~~

---

## Task 9: Register, authenticate, rotate, and durably queue Monobank webhooks

**Files:**

- Create: `src/contexts/banking/infrastructure/webhook_secret.rs`
- Modify: `src/contexts/banking/application/{commands.rs,handlers.rs,ports.rs}`
- Modify: `src/contexts/banking/infrastructure/{pg_unit_of_work.rs,pg_repositories.rs}`
- Modify: `src/contexts/banking/api/{dto.rs,handlers.rs,routes.rs}`
- Create: `tests/banking_webhook.rs`

- [ ] **Step 1 — RED: write webhook authentication and handshake tests**

Freeze the non-public callback contract as both `GET /webhooks/monobank/{webhook_credential}` for Monobank validation and `POST /webhooks/monobank/{webhook_credential}` for notification intake. Cover a CSPRNG-generated credential with at least 256 bits of entropy, per-connection uniqueness, encrypted-at-rest recovery for registration retry, keyed lookup digest persistence, constant-time verification, and rotation invalidating the old URL. After token validation activates a connection, prove the worker calls Monobank webhook registration with the generated callback URL, persists desired/registered versions and attempt state, retries registration failure after restart, and never schedules ordinary sync for an invalid token. Also cover generic 404 behavior for unknown/invalid credentials, provider validation handshake, body/size/content-type limits, replayed delivery, and disabled/revoked connection behavior. The callback is deliberately omitted from public OpenAPI because its path segment is a credential; a separate route-manifest/security test still makes method/path exact.

Capture tracing/access-proxy output and assert the raw request target, route credential, token, full callback URL, raw body, full external IDs, and merchant description are absent; logs use only the route template and redacted connection reference. A valid webhook response must not depend on Ledger availability.

- [ ] **Step 2: run and capture RED**

~~~bash
cargo test --test banking_webhook -- --nocapture
~~~

Expected: FAIL because webhook secret handling/routes do not exist.

- [ ] **Step 3 — GREEN: implement fast authenticated intake**

Expose a provider callback route inside the isolated V2 router using an unguessable per-connection path credential. Configure tracing so the raw path is not logged. After connection activation/secret creation or rotation, a durable worker calls the provider's registration endpoint and records registered URL version, attempt, retry time, and bounded/redacted failure; crash/restart safely retries the same desired version. The handler validates/limits the request, performs the provider handshake, resolves and verifies the connection, and transactionally stores encrypted provenance or normalized intake plus an outbox wake-up. Return promptly; normalization/import runs after receipt through the durable Banking workflow.

- [ ] **Step 4: run focused tests**

~~~bash
cargo test --test banking_webhook -- --nocapture
cargo test --test banking_sync -- --nocapture
~~~

Expected: PASS. Replays are harmless, and stopping/restarting the worker processes the persisted receipt.

- [ ] **Step 5 — REFACTOR: review credential exposure surfaces**

Check Axum error bodies, tracing spans, metrics labels, panic/debug formatting, database errors, and OpenAPI examples. The full callback URL is returned only once during connect/rotation; later reads expose `webhook_configured` and rotation time, not the credential.

- [ ] **Step 6: commit**

~~~bash
git add src/contexts/banking/infrastructure/webhook_secret.rs src/contexts/banking/application src/contexts/banking/infrastructure src/contexts/banking/api tests/banking_webhook.rs tests/banking_sync.rs
git commit -m "feat(banking): register and secure Monobank webhooks"
~~~

---

## Task 10: Expose Banking through the isolated replacement API and test harness

**Files:**

- Create: `src/contexts/banking/api/{mod.rs,dto.rs,handlers.rs,routes.rs}`
- Modify: `src/contexts/banking/mod.rs`
- Modify: `src/contexts/mod.rs`
- Modify: `src/api/v2.rs`
- Modify: `src/api/v2_state.rs`
- Modify: `src/bootstrap/v2.rs`
- Modify: `static/openapi.v2.json`
- Create: `tests/banking_api.rs`

- [ ] **Step 1 — RED: write isolated router contract tests**

Construct the application with `v2_test_db` and V2 test doubles. Cover authenticated tenant isolation and:

~~~text
POST /provider-connections/monobank
GET  /provider-connections
GET  /provider-connections/{id}
POST /provider-connections/{id}/disconnect
POST /provider-connections/{id}/credential-replacements
POST /provider-connections/{id}/webhook-rotations
GET  /provider-connections/{id}/resources
POST /provider-connections/{id}/resource-mappings
POST /provider-connections/{id}/resource-mappings/{mapping_id}/deactivations
POST /provider-connections/{id}/resource-mappings/{mapping_id}/replacements
POST /provider-connections/{id}/sync-jobs
GET  /sync-jobs/{id}
GET  /provider-events/{id}
GET  /accounting-processes/{id}
GET  /balance-observations/{id}
~~~

The provider-only callback routes are exactly `GET /webhooks/monobank/{webhook_credential}` and `POST /webhooks/monobank/{webhook_credential}`. They are mounted only in `src/api/v2.rs` before cutover, authenticate solely by the high-entropy path credential, return generic `404` on failure, and remain outside the public OpenAPI operation list while still being covered by route/security tests.

Financial/task POSTs require `Idempotency-Key`; mutable aggregate commands carry body `expected_version`. `credential-replacements` accepts the new X-Token only in the write body, returns `202` plus validation status, and never echoes/logs it; tests fence concurrent replacement/disconnect/sync races and preserve the connection ID. Mapping deactivation/replacement bodies additionally require a reason; replacement supplies a validated existing-account target or create-account intent and returns its durable process status. API tests prove a same-scope key with the same canonical hash returns the stored status/result without repeating validation/provider/outbox effects, while the same key with a different hash returns `409`. Those receipts live in `banking.command_receipts` and commit with the Banking aggregate/outbox. Amounts are decimal strings plus currency. Responses expose source, effective/recorded times, credential generation/status without secret material, revision, mapping history/review status, process status, and observation-to-Ledger-case link/status, but never token/webhook/raw payload/ciphertext. Ledger's existing isolated API alone owns `/reconciliations`, approval, and dismissal.

Also test the replacement `GET /accounts/{id}` composition: a mapped account combines Ledger balance/version with the latest Banking `provider_reported`, `available`, `reconciliation_difference`, currency, and explicit `as_of`; a manual/unmapped account returns those three provider fields as `null`. The composition calls both contexts' public query facades and performs no cross-context SQL join.

- [ ] **Step 2: run and capture RED**

~~~bash
cargo test --test banking_api -- --nocapture
~~~

Expected: FAIL because Banking routes are not composed into the isolated V2 router.

- [ ] **Step 3 — GREEN: implement API, composition, and bounded worker registration**

Mount the routes only in `src/api/v2.rs`. Compose account reads at the API boundary from Ledger and Banking public DTOs, preserving each source's `as_of` and nullable provider fields. Extend `bootstrap::v2` to accept Phase 1's `VerifiedV2Pool`/adapters and return the router plus bounded validation/discovery/sync/webhook-registration/intake worker handles. Tests must explicitly start/poll those handles; importing the module must not spawn tasks. Phase 8 promotes the already-tested composition to the default bootstrap.

Implement the account summary as an API/read-composition service over the two public façades. Never copy Ledger balance into a missing provider field, and choose the latest observation by stable `(observed_at, sequence, id)` order.

Update `static/openapi.v2.json` with the future replacement unversioned paths and security/error/idempotency/version contracts. Keep the default legacy router and runtime untouched.

- [ ] **Step 4: run API and OpenAPI tests**

~~~bash
cargo test --test banking_api -- --nocapture
cargo test --test openapi_v2 -- --nocapture
jq empty static/openapi.v2.json
~~~

Expected: PASS.

- [ ] **Step 5 — REFACTOR: prove the phase stayed parallel**

~~~bash
git diff --name-only -- src/main.rs src/api/routes.rs src/infrastructure/db.rs src/infrastructure/test_db.rs tests/common/mod.rs tests/migrations.rs Dockerfile docker-compose.yml .env .env.example
rg -n "MonobankService|PgBankConnectionRepository|Sqlite(Account|Transaction)Repository" src/api/v2.rs src/contexts/banking
~~~

Expected: both commands produce no matches/output. `DATABASE_URL` and the default migrator remain unchanged.

- [ ] **Step 6: commit**

~~~bash
git add src/contexts/banking/api src/contexts/banking/mod.rs src/contexts/mod.rs src/api/v2.rs src/api/v2_state.rs src/bootstrap/v2.rs static/openapi.v2.json tests/banking_api.rs
git commit -m "feat(api): add parallel v2 banking surface"
~~~

---

## Task 11: Prove the complete Phase 3 workflow and failure recovery

**Files:**

- Create: `tests/phase3_workflow.rs`
- Modify: `tests/banking_{persistence,monobank,sync,webhook,api,ledger_contract}.rs`
- Modify: `tests/context_boundaries.rs`

- [ ] **Step 1 — RED: write the full acceptance scenario**

On an isolated blank V2 database:

1. connect one encrypted X-Token;
2. discover a current/card resource and a jar while quarantining an unknown Monobank product;
3. bind one cash-like resource to an existing compatible Ledger account, create-and-map the other through the crash-safe process manager, and prove the provider-neutral `SecurityPortfolio` kind is unmappable in a domain test;
4. start a sync containing a duplicate, pending event, settled revision, monetary correction, reversal, and balance observation;
5. crash after Ledger commit but before Banking acknowledgment and restart all workers;
6. run two sync/import workers concurrently;
7. verify one financial effect per semantic revision, explicit reversal/replacement history, and cursor completion only after all page events finish;
8. verify provider observations did not change Ledger;
9. approve one fresh discrepancy and make another case stale with an intervening Ledger post;
10. fail provider webhook registration, restart and retry it, complete the validation handshake, replay an authenticated webhook, reject a wrong/rotated secret, then restart and process the durable receipt; and
11. disconnect while retaining inspectable history and eliminating usable credentials.

- [ ] **Step 2: run and capture RED**

~~~bash
cargo test --test phase3_workflow -- --nocapture
~~~

Expected: FAIL until all paths are integrated.

- [ ] **Step 3 — GREEN: close only integration gaps**

Fix composition, transaction boundaries, wait/poll helpers, and deterministic test scheduling. Do not weaken uniqueness, fencing, revision, authentication, or Ledger invariants to make the scenario pass.

- [ ] **Step 4: run the complete Phase 3 suite**

~~~bash
SQLX_OFFLINE=true cargo test --test v2_migrations -- --nocapture
cargo test --test banking_domain -- --nocapture
cargo test --test banking_persistence -- --nocapture
cargo test --test banking_monobank -- --nocapture
cargo test --test banking_sync -- --nocapture
cargo test --test banking_webhook -- --nocapture
cargo test --test banking_api -- --nocapture
cargo test --test banking_ledger_contract -- --nocapture
cargo test --test phase3_workflow -- --nocapture
cargo test --test context_boundaries -- --nocapture
cargo test --test integration_runtime -- --nocapture
cargo test --test ledger_persistence -- --nocapture
cargo test --test openapi_v2 -- --nocapture
cargo test
~~~

Expected: PASS.

- [ ] **Step 5 — REFACTOR: final security and architecture audit**

~~~bash
rg -n "set_balance|adjust_balance|UPDATE ledger\.|DELETE FROM ledger\.|ON CONFLICT.*DO NOTHING" src/contexts/banking src/integration/process_managers src/infrastructure/migrations_v2/0004_banking.sql
rg -n "X-Token|token\s*=|raw(_payload|_body)?|webhook.*secret" src/contexts/banking tests/banking_* tests/phase3_workflow.rs
rg -n "crate::contexts::ledger::(domain|application|infrastructure)|FROM ledger\.|JOIN ledger\." src/contexts/banking src/integration/process_managers
git diff --check
~~~

Expected: no direct balance/posted-row mutation, no blanket duplicate discard, no secrets/raw fixture data in logs or snapshots, no Ledger-internal/table coupling, and no whitespace errors. Manually inspect legitimate type/fixture declarations returned by the broad secret scan.

- [ ] **Step 6: commit**

~~~bash
git add tests/phase3_workflow.rs tests/banking_*.rs tests/context_boundaries.rs
git commit -m "test(banking): prove revision-safe Monobank workflow"
~~~

---

## Verification commands

Task 11 is the canonical Phase 3 gate. Run its complete suite plus the security/architecture searches; the minimum gate is:

~~~bash
SQLX_OFFLINE=true cargo test --test v2_migrations -- --nocapture
cargo test --test banking_domain -- --nocapture
cargo test --test banking_persistence -- --nocapture
cargo test --test banking_monobank -- --nocapture
cargo test --test banking_sync -- --nocapture
cargo test --test banking_webhook -- --nocapture
cargo test --test banking_api -- --nocapture
cargo test --test banking_ledger_contract -- --nocapture
cargo test --test phase3_workflow -- --nocapture
cargo test --test context_boundaries -- --nocapture
cargo test --test integration_runtime -- --nocapture
cargo test --test openapi_v2 -- --nocapture
cargo test
~~~

## Commit boundaries

Keep these independently reviewable boundaries; do not collapse schema, secret handling, provider normalization, financial import, reconciliation, webhook security, and API composition into one commit:

1. `feat(banking): model provider connections and durable facts`
2. `feat(db): add strict v2 banking schema`
3. `feat(banking): encrypt connections and discover Monobank resources`
4. `feat(banking): validate and audit ledger resource mappings`
5. `feat(banking): persist revision-aware provider events`
6. `feat(banking): add durable fenced Monobank sync`
7. `feat(banking): import provider revisions idempotently`
8. `feat(banking): deliver provider balance observations`
9. `feat(banking): register and secure Monobank webhooks`
10. `feat(api): add parallel v2 banking surface`
11. `test(banking): prove revision-safe Monobank workflow`

Every boundary must compile and its focused tests must pass. Before Phase 8, reverting any Phase 3 commit must not require changing the legacy database, default runtime, public API, Docker state, or environment.

## Exit criteria

- [ ] Migration `0004_banking.sql` applies only through the guarded V2 migrator on a blank/already-marked V2 database.
- [ ] One encrypted-only Monobank X-Token connection discovers separate card/current-account and jar resources; secrets are absent from database plaintext, API reads, debug output, fixtures, and logs.
- [ ] A `NeedsReauth`/proactive credential replacement is exact, idempotent, expected-version fenced, validates before activation, preserves connection/resource/mapping identity, and prevents stale old-generation workers from advancing state.
- [ ] Provider-neutral `SecurityPortfolio` resources cannot map to Ledger scalar accounts. Monobank tests discover cards/current accounts and jars only, and unknown Monobank products are quarantined rather than fabricated as securities/ОВДП.
- [ ] Mapping validates tenant, native currency, lifecycle, kind/nature, authority, and version through Ledger's public façade; create-and-map is durable/idempotent and bind-existing uses no cross-context SQL or repository import.
- [ ] Mistaken or `NeedsReview` mappings can be version-checked/deactivated or replaced through exact task routes; history/effective boundaries remain visible and prior Ledger journals are not moved.
- [ ] Durable event identity/revisions correctly handle duplicate delivery, pending-to-settled, monetary correction, reversal, and conflicting content.
- [ ] Sync jobs survive restart, honor per-token rate limiting, fence concurrent workers, retry safely, and advance a cursor only after all page events are processed or explicitly quarantined.
- [ ] Import uses the shared inbox/process-manager runtime and Ledger idempotency so duplicate delivery and crash-after-Ledger-commit have one financial effect.
- [ ] Provider snapshots/observations are stored in Banking, passed through Ledger's provider-neutral `ObserveProviderBalance` command, and never overwrite Ledger. Ledger alone owns the resulting case; approval creates a visible, version-checked reconciliation `JournalEntry`, and stale cases cannot apply an obsolete delta.
- [ ] Per-connection webhook credentials are high entropy, rotatable, constant-time verified, validation-handshake compatible, replay safe, and absent from normal logs. Provider registration state/attempts survive failure and restart; receipt remains independent of Ledger availability.
- [ ] Users can inspect Banking connections, resources, mappings, provider revisions, sync jobs/pages, accounting processes, observations, and links to Ledger-owned reconciliation cases through tenant-safe V2 reads; case decisions remain Ledger reads.
- [ ] Mapped account reads compose Ledger and the latest Banking observation with explicit `as_of`; manual/unmapped accounts return nullable provider fields without invented values.
- [ ] Banking tests use `v2_test_db` and the isolated V2 router only.
- [ ] `src/main.rs`, default DB/test migrators, legacy public routes/API, Docker, environment files, and `DATABASE_URL` are unchanged; Phase 8 remains the sole cutover.
- [ ] The context-boundary, Ledger regression, integration-runtime, OpenAPI, complete Phase 3, and full test suites pass.

## Explicitly out of scope

- Wiring the V2 router/workers into the default application or changing `DATABASE_URL`/Docker volumes.
- Copying legacy Monobank connections, tokens, mappings, transactions, balances, cursors, or webhook registrations.
- Deleting or modifying legacy Monobank code/routes before Phase 8.
- Treating provider-reported balances as authoritative Ledger balances.
- Broker integration, automatic ОВДП import, live securities prices, tax lots, or Portfolio valuation; Phase 7 owns Portfolio/ОВДП behavior.
- Providers other than Monobank. The provider-neutral ports and model make later adapters possible, but Phase 3 implements and verifies only Monobank.
- Cross-context joins, distributed transactions, dual writes, compatibility views, or a generic public postings API.
