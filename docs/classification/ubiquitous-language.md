# Ubiquitous Language — Categories and Transaction Classification

| Term | Definition | Code / concept |
|---|---|---|
| Category taxonomy | One user's versioned, ordered hierarchy of category nodes. | `CategoryTaxonomy` |
| Category node | A named, styled entity inside a taxonomy; it may be a group or a leaf. | `CategoryNode` |
| Assignable category | An effectively active leaf whose kind accepts the transaction cash-flow kind. | `require_assignable` |
| Local lifecycle | The node's own `active` or `archived` state. | `CategoryLifecycle` |
| Effective lifecycle | Active only when the node and every ancestor are locally active. | taxonomy query |
| Assignment origin | The intent that produced the current category: manual, recurring, AI, or none. | `CategoryAssignmentOrigin` |
| Automation state | Whether AI may act: eligible, explicitly suppressed, or ambiguous legacy state. | `AutomationState` |
| Classification evidence | The minimized transaction facts supplied to a classifier, without tenant or provider identifiers. | `ClassificationEvidence` |
| Classification target | Durable queued work for one transaction/evidence generation. | `ClassificationTarget` |
| Classification decision | An auditable parsed prediction and its lifecycle, fenced by taxonomy and annotation versions. | `ClassificationDecision` |
| Review suggestion | A decision requiring user acceptance, correction, or rejection. | `review_pending` |
| Feedback example | Tenant-private positive or negative evidence derived from explicit user intent. | `ClassificationExample` |
| Backfill | A user-requested date-range scan that queues eligible historical transactions. | `ClassificationBackfill` |

## Disambiguation notes

- A category changes transaction metadata, never journal postings or balances.
- `archived` is a local node state; `effectively archived` can also result from an archived ancestor.
- `uncategorized` does not imply `eligible`: an explicit manual clear is uncategorized and suppressed.
- A suggestion is not an applied category. Ledger remains the source of truth for application.
- Confidence is a model-provided score used by policy and quality evaluation, not a guarantee of correctness.

