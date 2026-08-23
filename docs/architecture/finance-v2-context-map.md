# Finance V2 context map

Finance V2 is a modular monolith with schema ownership. A context may access
its own schema and stable Shared Kernel/reference identifiers. Cross-context
behavior uses a context's `public` module, versioned integration events, or a
process manager; it never imports another context's domain, application,
repository, API, or infrastructure module and never queries another private
schema.

| Owner | Schema/module | Public capability and published facts |
|---|---|---|
| Shared Kernel | `shared_kernel`, `src/shared_kernel` | Tenant/user IDs, money, currency codes, event metadata, idempotency primitives, and the immutable `finance-v2` marker. |
| Integration Runtime | `integration`, `src/integration` | Transactional outbox/inbox, leased process state, retries, deduplication, and process managers that call context façades only. |
| Reference Data | `reference_data`, `contexts/reference_data` | Currency/FX queries and `reference-data.fx-observed.v1`. |
| Classification | `classification`, `contexts/classification` | Tenant-scoped category catalog used through its façade. |
| Preferences | `preferences`, `contexts/preferences` | Version-fenced user base-currency preferences. |
| Ledger | `ledger`, `contexts/ledger` | Account/journal/reconciliation commands and queries; publishes versioned account, journal, balance, annotation, reconciliation, and internal-accounting facts. |
| Banking | `banking`, `contexts/banking` | Provider connections, encrypted credentials, discovered resources, mappings, webhook receipts, sync pages, and reconciliation observations; requests Ledger effects through `LedgerFacade`. |
| Mail | `mail`, `contexts/mail` | Gmail connections, encrypted source-message revisions, parser attempts, and immutable receipt evidence; publishes `mail.receipt-evidence-recorded.v1`. |
| Recurring | `recurring`, `contexts/recurring` | Subscription lifecycle and append-only evidence/matching; publishes charge evidence, match, and unmatch facts. |
| Sharing | `sharing`, `contexts/sharing` | Contacts, bills, allocations, payer shares, and settlements; publishes accounting requests, position changes, settlements, and cancellations. |
| Loans | `loans`, `contexts/loans` | Borrowed/lent agreements, terms, immutable movements, reversals, and closures; publishes the `loans.*.v1` agreement/accounting lifecycle. |
| Portfolio | `portfolio`, `contexts/portfolio` | Instruments, accounts, lots, immutable transactions, positions, valuations, and cash-settlement state; publishes the `portfolio.*.v1` lifecycle. |
| Reporting | `reporting`, `contexts/reporting` | Rebuildable projections and query models for Ledger, Sharing, Loans, and Portfolio facts; owns consumed-event receipts and visible dead letters. |

## Allowed dependency direction

HTTP adapters map requests into their owning context's public commands and
queries. Context application code depends inward on its own domain and on
ports. Infrastructure implements those ports. The composition root constructs
concrete adapters after the database lineage is verified. Process managers
coordinate public façades and versioned event payloads; Reporting consumes
published facts and can rebuild from them.

Schema-qualified foreign keys to stable immutable identifiers are allowed only
where the migration architecture explicitly defines them. They do not grant
query or write access to the referenced owner's tables. Repository types,
private table layouts, and context-private errors/aggregates are never shared.

`tests/context_boundaries.rs` rejects private imports and foreign-schema SQL.
`scripts/check_no_legacy_finance_sql.sh` rejects legacy unqualified SQL and
private-context imports from integration code. Any required new dependency
must first become an explicit public contract or versioned event.
