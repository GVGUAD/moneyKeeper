# Domain Context: Categories and Transaction Classification

## Problem statement

Moneykeeper users need a stable personal taxonomy for income and expense transactions, plus assistance classifying new and historical cash flows without losing explicit user intent. The system must keep category administration independent from immutable accounting, make every automatic decision auditable, and never delay recording a transaction while an external model runs.

## Domain vision

Classification is a supporting subdomain implemented with light DDD. A versioned `CategoryTaxonomy` protects the structural rules of a user's tree; a separate `ClassificationDecision` records one model judgment. Ledger remains authoritative for the category applied to a transaction and enforces the priority `manual > recurring > ai`.

## Key scenarios

1. A user creates, styles, moves, reorders, archives, or restores a category within one version-fenced taxonomy change.
2. A user assigns or clears a category on an ordinary cash-flow transaction; Ledger validates the referenced leaf and records user precedence.
3. A new uncategorized manual or provider-imported cash flow queues evidence without waiting for AI.
4. A classifier records an abstention, review suggestion, or pending automatic application against observed taxonomy and annotation versions.
5. A user accepts, corrects, or rejects a suggestion; a durable policy applies the resulting Ledger intent and records private feedback.
6. A date-range backfill queues eligible historical transactions behind live work and pauses at the daily call cap.

## Subdomain classification

- Type: Supporting
- DDD posture: Light DDD
- Deployment: Existing modular monolith; no new service

## Bounded contexts

| Context | Responsibility | Subdomain type |
|---|---|---|
| Classification | Category taxonomy, predictions, decisions, review state, examples, backfill, quota | Supporting |
| Ledger | Immutable journals and authoritative mutable transaction annotation | Core |
| Banking | Provider transaction facts and merchant evidence | Supporting |
| Recurring | Subscription evidence and recurring-owned assignment intent | Supporting |
| Reporting | Eventually consistent financial/category projections | Supporting |
| Integration Runtime | Durable published-event delivery and cross-context policies | Generic |

## Event-storming result

The PascalCase facts below are conceptual modeling names, not additional published event contracts. Taxonomy changes and decision transitions are persisted within Classification; only the versioned Ledger and Banking events listed here cross context boundaries. See the bounded-context canvas for implemented coordination.

| Command or policy | Aggregate | Resulting fact |
|---|---|---|
| Create / edit / move / reorder / archive / restore category | `CategoryTaxonomy` | `CategoryTaxonomyChanged` |
| Classify transaction evidence | `ClassificationDecision` | `ClassificationSuggested`, `ClassificationAbstained`, or `CategoryApplicationRequested` |
| Resolve suggestion | `ClassificationDecision` | `ClassificationResolutionRequested` |
| Apply category assignment | Ledger `TransactionAnnotation` | `ledger.category-assignment-changed.v1` |
| Complete provider import | Banking provider-import process | `banking.provider-transaction-imported.v1` |

## Aggregate candidates

- **CategoryTaxonomy** — one per user; protects depth, cycles, parent-kind containment, active sibling-name uniqueness, dense sibling order, lifecycle, and global optimistic version.
- **ClassificationDecision** — one model judgment for one transaction/evidence generation; protects status transitions, confidence outcome, observed versions, and resolution idempotency.
- **TransactionAnnotation** (Ledger, existing) — owns the applied category, provenance, automation state, compare-and-swap version, and precedence.

## Integrations

- Banking → Classification: Customer–Supplier through `banking.provider-transaction-imported.v1` and a provider-evidence public capability.
- Ledger → Classification: Open Host Service + Published Language through journal/category-assignment facts and Ledger query capabilities.
- Classification → Ledger: Customer–Supplier through a version-fenced `ApplyCategoryAssignment` public capability.
- Recurring → Ledger: Customer–Supplier through the same assignment capability with recurring provenance.
- Ledger → Reporting: Open Host Service + Published Language through `ledger.category-assignment-changed.v1`.
- OpenAI → Classification: Anticorruption Layer implemented by the provider-neutral `TransactionClassifier` port and Responses adapter.

## Related artifacts

- [Ubiquitous language](ubiquitous-language.md)
- [Context map](context-map.puml)
- [Classification canvas](bounded-context-classification.md)
- [Tactical class diagram](class-diagram-classification.puml)
