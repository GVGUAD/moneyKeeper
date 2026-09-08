# Bounded Context: Classification

## Responsibility

Classification owns each user's category taxonomy and the lifecycle of automatic transaction-classification work. It does not own transaction balances, applied annotations, provider credentials, or reporting projections.

## Subdomain type

Supporting subdomain; light DDD.

## Ubiquitous language

The canonical terms are defined in [ubiquitous-language.md](ubiquitous-language.md). In particular, taxonomy, decision, target, suggestion, and applied assignment are distinct concepts.

## Aggregates

| Aggregate | Protects invariant | Commands | Durable coordination |
|---|---|---|---|
| `CategoryTaxonomy` | One valid depth-3 ordered tree at one global version | create, edit, move, reorder, archive, restore | Version increment and stale-target fencing in the taxonomy transaction |
| `ClassificationDecision` | One valid prediction/resolution state machine for observed input versions | record prediction, request accept/correct/reject, mark applied/stale | Persisted decision lifecycle and leased application targets |

## Entities and value objects

- Entity: `Category`, identified only within its taxonomy.
- Typed identities and values include `CategoryId`, `ClassificationDecisionId`, `Confidence`, `BackfillRange`, and validated `ClassificationEvidence`. Names, colors, icon keys, positions, paths, and versions use validated primitive fields rather than additional wrapper types.
- External identities are referenced only by `UserId`, `JournalEntryId`, and an opaque annotation version.

## Dependencies

- Upstream Banking and Ledger publish facts and expose public read capabilities.
- Downstream Ledger accepts category-application intent through its public capability.
- OpenAI is isolated behind an anticorruption-layer adapter implementing `TransactionClassifier`.
- Reporting never reads Classification storage; it follows Ledger's applied-assignment fact.

## Invariants and hotspots

- One taxonomy mutation locks and versions one user's taxonomy; no cross-user tree access is possible.
- Model calls never hold a database transaction or event-feed receipt open.
- A model result is stale when either observed taxonomy or annotation version changed.
- User intent is learned only inside that tenant and is never overwritten by lower-priority automation.
- Provider correction inheritance is coordinated outside both aggregates and applied as a separate Ledger transaction.
