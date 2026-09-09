# Domain Context: Banking and Provider Imports

## Problem statement

Moneykeeper users need bank activity and balances to arrive reliably despite duplicate deliveries, provider corrections, interrupted synchronization, and credential changes. A provider's resource identifiers, transaction states, and balance semantics must be translated before they can become accounting facts.

This set describes the current source implementation. The code remains authoritative when behavior changes.

## Domain vision

Banking is a supporting subdomain in the modular monolith. Its provider-neutral domain models protect connection, resource-mapping, event-revision, synchronization, and observation rules. Private PostgreSQL adapters also enforce durable workflow transitions and fencing; the domain structs alone are not a complete model of runtime orchestration.

Banking owns provider evidence and import progress. Ledger owns accounts, immutable journals, balance projections, and reconciliation decisions. A successful provider request or accepted webhook does not imply that accounting has completed.

## Key scenarios

1. A user submits a Monobank credential; a worker validates it, discovers resources, and registers a secret webhook callback.
2. The user explicitly maps a supported card, current account, or jar to a compatible existing Ledger account, or requests creation of a provider-observed account.
3. Statement synchronization and webhooks converge on normalized provider revisions. Repeated content is deduplicated; conflicting content for an explicit revision is quarantined.
4. Durable import work waits for a mapping and prior revisions, then calls Ledger with stable command identities. Monetary corrections and reversals append accounting history.
5. Comparable balance observations reach Ledger reconciliation. Credit-balance semantics that cannot be safely compared remain marked for review.
6. A user replaces a credential, rotates the callback secret, changes a mapping, or disconnects while workers may be in flight. Versions, generations, and leases fence stale work.

## Subdomain classification

- Type: Supporting
- DDD posture: Provider-neutral domain models plus durable application workflows and an anticorruption layer
- Deployment: Existing modular monolith; Monobank is the implemented provider adapter

## Bounded contexts

| Context | Responsibility | Subdomain type |
|---|---|---|
| Banking | Connections, credentials, resources, mappings, provider revisions, synchronization, observations | Supporting |
| Ledger | Accounts, immutable accounting, authoritative annotations, reconciliation | Core |
| Reference Data | Known/enabled currencies, numeric currency codes, minor units | Supporting |
| Classification | Classification work using imported-transaction facts and provider evidence | Supporting |
| Integration Runtime | Worker supervision, outbox delivery, cross-context process managers | Generic |

Reporting follows Ledger accounting facts; Banking does not write report balances. `SecurityPortfolio` produces a route-to-Portfolio decision, not an implemented securities import workflow.

## Event-storming result

The first column describes commands or policies. Lifecycle descriptions are conceptual facts unless an exact versioned event name is shown.

| Command or policy | Model / durable work | Resulting fact |
|---|---|---|
| Connect / replace credential / validate | `ProviderConnection`, validation claim | Connection requested, candidate accepted or rejected, resources discovered |
| Bind / create and map / deactivate mapping | `ExternalResource`, mapping intent | Mapping activated or ended with retained history |
| Receive callback | Encrypted webhook receipt | `banking.webhook-received.v1` |
| Normalize statement or webhook / intake revision | `ProviderEvent`, separate processing row | `banking.provider-event-ready.v1` for new evidence |
| Import provider revision | Provider-import process, Ledger commands | `banking.provider-transaction-imported.v1` when completion records `posted` with a journal ID |
| Record provider balance | `BalanceObservation`, delivery row | `banking.balance-observed.v1` |
| Deliver comparable observation | Observation process, Ledger reconciliation | Delivery link or older-observation outcome |
| Request / fetch / finalize synchronization | `SyncJob`, pages and event links | Progress advances after all page events reach accepted terminal outcomes |

Published events and database polling are complementary mechanisms. The Banking worker directly claims import and observation work; an outbox event name does not imply that an event consumer is the sole scheduler.

## Integrations

- Monobank → Banking: Anticorruption Layer through `ProviderClient` and the private `ProviderNormalizer`; numeric amounts, currencies, product types, and holds become provider-neutral facts.
- Reference Data → Banking: Customer–Supplier through the public currency catalog.
- Banking → Ledger: Customer–Supplier through public account-binding, provider-account creation, import, reversal, and observation capabilities. Cross-context import and observation policies live in Integration Runtime.
- Banking → Classification: Open Host Service + Published Language through `banking.provider-transaction-imported.v1` and tenant-scoped provider-evidence reads. Ledger remains authoritative for the applied category.

## Related artifacts

- [Ubiquitous language](ubiquitous-language.md)
- [Banking canvas](bounded-context-banking.md)
- [Context map](context-map.puml)
- [Tactical class diagram](class-diagram-banking.puml)
- [Operations](operations.md)
- [Application context map](../architecture/context-map.md)
- [Ledger guide](../ledger.md)
