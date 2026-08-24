# Moneykeeper context map

Moneykeeper is a modular monolith with explicit bounded contexts and schema
ownership. HTTP adapters call only their owning context's `public` contract.
Public facades are concrete, cloneable types that type-erase private application
capabilities with `Arc<dyn Capability>`. Application services depend inward on
their domains and narrow ports; PostgreSQL repositories, provider clients,
encryption, and transport adapters implement those ports.

The immutable database lineage is still identified by the operational literal
`finance-v2`. That value is a compatibility marker, not an architectural or Rust
API version.

| Owner | Schema/module | Public capability and published facts |
|---|---|---|
| Shared Kernel | `shared_kernel`, `src/shared_kernel` | Tenant/user IDs, money, currency codes, event metadata, and idempotency primitives. |
| Integration Runtime | `integration`, `src/integration` | Transactional outbox/inbox, retries, leased workers, and event delivery. |
| Reference Data | `reference_data`, `contexts/reference_data` | Currency and FX catalogs through `CurrencyRepository` and `FxObservationRepository`; publishes `reference-data.fx-observed.v1`. |
| Classification | `classification`, `contexts/classification` | Tenant category taxonomy through `CategoryRepository`. |
| Preferences | `preferences`, `contexts/preferences` | Version-fenced base-currency preferences through `PreferencesRepository`. |
| Ledger | `ledger`, `contexts/ledger` | Account, journal, reconciliation, and internal-accounting capabilities around aggregate/UoW ports; publishes exact journal, balance, annotation, reconciliation, and accounting facts. |
| Banking | `banking`, `contexts/banking` | Connection, resource, provider-event, sync-job, observation, credential, provider, Ledger, and currency ports. Concrete Monobank, cipher, webhook, and PostgreSQL adapters are private or outer adapter APIs. |
| Mail | `mail`, `contexts/mail` | Connection and message repositories plus Gmail/OAuth ports; publishes `mail.receipt-evidence-recorded.v1`. |
| Recurring | `recurring`, `contexts/recurring` | Subscription, evidence, and matching repositories plus Ledger annotation; publishes charge evidence, match, and unmatch facts. |
| Sharing | `sharing`, `contexts/sharing` | Contact, bill, settlement, and accounting-workflow repositories plus Ledger accounting; publishes accounting requests, positions, settlements, and cancellations. |
| Loans | `loans`, `contexts/loans` | Agreement, movement, and accounting-workflow repositories plus Ledger accounting; publishes agreement and movement lifecycle facts. |
| Portfolio | `portfolio`, `contexts/portfolio` | Account/instrument, transaction/lot, valuation, and cash-settlement persistence plus Ledger settlement capability; publishes transaction, position, valuation, and cash-settlement facts. |
| Reporting | `reporting`, `contexts/reporting` | Projection writer, report query, and projection-rebuild capabilities over published facts. |

## Dependency rule

```text
HTTP API -> public façade -> application use case -> domain
                    \             |
                     \         inward port
                      \            ^
                       +-- composition root -- infrastructure adapter
```

A context may import another context only through that context's `public`
module. It may query or mutate only its own private schema. Stable foreign-key
references defined by migrations do not grant runtime access to the foreign
schema. Expected business failures stay in domain errors; façade errors classify
not-found, conflict, invalid, and persistence outcomes without exposing SQLx.

## Independent event policies

The PostgreSQL event feed is shared, but delivery progress is independent:

- `RecurringEventConsumer` handles Mail receipt evidence and exact Ledger journal
  facts. Its durable receipt identity is `recurring-event-policy-v1`.
- `ReportingEventConsumer` handles exact Ledger, FX, Recurring, Sharing, Loans,
  and Portfolio published facts. Its durable receipt identity is
  `reporting-projections-v1`.

Each consumer reads the earliest event missing its own receipt. Unknown events
are acknowledged as ignored. Known events are schema-checked, applied through a
public façade, and acknowledged only after the idempotent effect commits.
Malformed known facts remain retryable and block only the affected consumer.
Migration 0012 seeds both receipt identities from the retired router's durable
history and retains the original receipts for rollback diagnostics.

The checked-in [`context-map.svg`](context-map.svg) renders
[`context-map.puml`](context-map.puml). Architecture enforcement lives in
`tests/context_boundaries.rs`; legacy SQL enforcement lives in
`scripts/check_no_legacy_finance_sql.sh`.
