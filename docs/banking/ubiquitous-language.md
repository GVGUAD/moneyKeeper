# Ubiquitous Language — Banking and Provider Imports

| Term | Definition | Code / concept |
|---|---|---|
| Provider connection | One tenant's provider identity, credential lifecycle, and version. | `ProviderConnection` |
| Connection version | Optimistic version used for connection mutations and captured by sync work. | `ConnectionVersion` |
| Credential generation | Generation of provider authentication material used to fence work and bind encryption. | `credential_generation` |
| Candidate credential | Replacement awaiting validation before promotion; distinct from the retained active credential. | pending credential envelope |
| Credential envelope | Authenticated encrypted material with a key identifier, nonce, and envelope version. | `CredentialEnvelope` |
| Webhook credential | Random secret in the callback path, looked up by a keyed digest; independent of the provider token. | `WebhookCredential` |
| Webhook receipt | Durable encrypted callback payload awaiting normalization; duplicate deliveries share a receipt. | `banking.webhook_receipts` |
| External resource | A discovered provider product with stable internal identity, currency, funding model, and mapping history. | `ExternalResource` |
| Provider resource ID | Provider-assigned resource identifier, distinct from the internal UUID used in API commands. | `provider_resource_id` in resource views |
| Funding model | Own funds, revolving credit, or unknown; controls mapping and balance comparability. | `FundingModel` |
| Resource mapping | Audited association between an external resource and a Ledger account. | `ResourceMapping`, `ResourceMappingView` |
| Mapping intent | Durable pending account-creation association completed through Ledger's public contract. | `pending_account_creation` |
| Provider event | Immutable normalized evidence for one provider transaction revision. | `ProviderEvent` |
| Revision identity | Connection, resource, external event ID, and positive revision number. | `ProviderEventIdentity` |
| Intake outcome | Whether an explicit revision is new, an identical duplicate, or conflicting content. | `ProviderEventIntakeOutcome` |
| Transaction state | Provider lifecycle: pending, settled, or reversed. | `ProviderTransactionState` |
| Processing state | Accounting progress for a revision, separate from its provider lifecycle. | `EventProcessingState`, provider-event process row |
| Operation money | Signed account-currency effect used for accounting. Original purchase-currency money is separate evidence. | `operation_money`, `original_money` |
| Sync job | Durable request for a resource and time range, with a captured connection version and credential generation. | `SyncJob`, `SyncJobView` |
| Sync page | One fetched statement window and its durable event links and completion counts. | `SyncPageView` |
| Lease / fencing token | Expiring worker claim and monotonically advanced token checked before completion. | `SyncLease`, persisted lease fields |
| Balance observation | Immutable provider amount, basis, time, and resource sequence, with separate delivery progress. | `BalanceObservation` |
| Balance basis | Meaning of an observed amount: reported, available, credit limit, or statement running balance. | `BalanceBasis` |
| Comparable balance | An amount whose semantics can be compared with Ledger; otherwise a reason is retained. | `BalanceComparability` |
| Reconciliation case | Ledger-owned comparison and user decision, referenced by observation delivery. | `ReconciliationCaseId`, `ObservationDelivery` |
| Accounting process | Durable coordination status exposed for inspection; distinct from an immutable journal. | `AccountingProcessView` |

## Disambiguation notes

- Connection state, credential-validation state, and webhook-registration state are separate. `active` alone does not prove successful callback registration.
- Mapping mutations use the external resource's `version`, not the connection version or `current_mapping.mapping_version`.
- Provider `pending` is not a promise of deferred accounting: the current import policy can post a pending revision. A later unchanged settlement can reuse the journal.
- A completed sync page can include quarantined events. Completion does not certify a reconciled balance or an error-free import.
- A provider observation never directly sets a Ledger balance. Ledger journals and approved reconciliation adjustments produce that balance.
- A `masked_label` is display data, not a secrecy guarantee; the adapter can fall back to an IBAN. Keep it out of operational logs.

See the [Banking canvas](bounded-context-banking.md) for invariants and [operations](operations.md) for runtime behavior.
