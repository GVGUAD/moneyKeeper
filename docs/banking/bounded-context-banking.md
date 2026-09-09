# Bounded Context: Banking

## Responsibility

Banking owns tenant-scoped provider connections, encrypted credentials and callback receipts, discovered resources, mapping history, normalized provider revisions, sync progress, and provider balance observations. Ledger owns the resulting accounts, journals, annotations, balances, and reconciliation decisions.

## Subdomain type

Supporting subdomain. Provider-neutral domain models and application ports isolate Monobank and PostgreSQL. Durable orchestration rules also live in repository transactions; the model is not solely an in-memory aggregate implementation.

## Ubiquitous language

See [ubiquitous-language.md](ubiquitous-language.md). Provider transaction state, import processing state, and Ledger accounting state are distinct.

## Aggregates and durable models

| Model | Protects invariant | Commands / behavior | Durable coordination |
|---|---|---|---|
| `ProviderConnection` | Versioned lifecycle; at most one active and one candidate credential | request, activate, replace, accept/reject candidate, disconnect | Validation and webhook claims carry generations, versions, and leases |
| `ExternalResource` with `ResourceMapping` entities | Compatible mapping, at most one active mapping, retained mapping history | discover, map, deactivate; currency changes restricted after mapping/import | Existing-account validation via Ledger; pending creation intent before provider-account creation |
| `ProviderEvent` with `ProviderEventIdentity` | Immutable normalized revision identity and content | record, derive next revision, compare revisions | Separate process state, conflict records, journal links, causal import claims |
| `SyncJob` with `SyncLease` | Valid range, live fenced claim, complete event counts before progress | request, claim, begin/complete page | Resource snapshot, fetched windows, page-event links, retry scheduling |
| `BalanceObservation` | Valid source sequence/time and same-currency comparable amount | record, deliver | Separate delivery state and reconciliation link; non-comparable evidence retained |

## Entities and value objects

`ResourceMapping` belongs to `ExternalResource`. Connections, resources, events, sync jobs, and observations have distinct typed IDs. Values include `ConnectionVersion`, `CredentialEnvelope`, `ProviderEventIdentity`, `ResourceKind`, `FundingModel`, `BalanceBasis`, and `BalanceComparability`, plus Shared Kernel `UserId`, `Money`, and `CurrencyCode`.

Ledger objects are referenced by public IDs and command/query contracts. Sync pages, webhook receipts, validation attempts, and delivery/process rows are durable coordination records, not additional domain aggregate structs. The [tactical diagram](class-diagram-banking.puml) reflects that distinction.

## Public capabilities and dependencies

- `BankingFacade` exposes connection lifecycle, resource mapping, provider intake/evidence, synchronization, observation, and worker capabilities. HTTP handlers enter through `banking::public`.
- Private ports divide connection, resource, provider-event, sync-job, observation, webhook, and worker persistence. The shared PostgreSQL store implements them using Banking-owned transactions.
- `ProviderClient` and `CredentialCipher` are provider/cryptography capabilities; the private normalizer translates provider payloads. Concrete Monobank, cipher, webhook-secret, and database adapters are wired at the composition root.
- Ledger's public facade validates existing accounts and creates provider-observed accounts. Integration process managers import/reverse revisions and deliver observations through public facades without writing Ledger tables.
- Reference Data supplies known currencies and minor units. Classification consumes the published imported-transaction fact and public evidence reads. See the [context map](context-map.puml).

## Invariants and hotspots

- All persistence and Ledger bindings are tenant-scoped. Resource and mapping HTTP paths also validate the owning connection.
- Own-funds cards map to debit-card assets, current accounts to current assets, jars to jar assets, and revolving-credit cards to credit-card liabilities. Ledger validates currency, lifecycle, and account compatibility. Unsupported/unknown products require review; securities are not mapped as cash.
- Explicit revision intake distinguishes identical content from conflicting content. Monobank normalization can assign successive revisions when evidence changes. Import work waits for an eligible mapping and earlier unfinished revisions.
- Ledger calls have stable source/idempotency identities. A crash after accounting commits can replay the same effect before Banking records completion. Monetary corrections reverse prior accounting and import a new journal; original journals remain immutable.
- Correction category inheritance preserves manual or recurring intent where valid and preserves protected clears; AI assignments are not copied. This policy coordinates separate Ledger commands outside Banking's domain entities.
- Worker claims commit before provider I/O. Completion checks lease ownership, token, expiry, and relevant connection/credential state. Provider retries are bounded; accounting delivery is a separate workflow.
- Runtime page finalization accepts `posted`, `no_financial_change`, and `quarantined` event outcomes. A `terminal_failure` process still blocks that finalizer.
- Comparable observations create Ledger reconciliation work rather than balance-setting commands. The current adapter marks revolving-credit balance semantics non-comparable.
- Mapping replacement deactivates the old mapping before binding/creating the replacement. It is not one atomic cross-context transaction; inspect state after failure.

See [operations](operations.md) for API retry caveats, connection recovery, and diagnostic reads.
