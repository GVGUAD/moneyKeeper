# Transfer conversion

Ledger owns exact account movements, conversion lifecycle, restoration links, workflow claims, receipts, projections, audit and outbox events. Banking owns provider evidence. This extends the existing Rust modular monolith and its rich accounting model; it does not introduce a new service or a cross-context aggregate.

A conversion identifies one or two live ordinary manual/imported journals, their immutable metadata, the selected date, and the generated transfer/reversal journals. Preview normalizes transfer principal without changing the recorded account movements. Its token includes the input, source annotations, account versions and balance versions. Commit revalidates while holding the Ledger unit of work. Source journals remain directly addressable for audit.

For an outgoing 102 and incoming 100 in the same currency, the caller must explicitly supply and confirm a fee of 2. The transfer principal is 100 and the fee remains an expense. For FX, a source-currency fee is subtracted from outgoing principal; a target-currency fee is added to incoming principal. Displayed source-per-target rate is derived from these exact principals. No posting is calculated from a rounded rate. Existing transfer posting rules and per-currency FX clearing accounts are shared by both workflows.

Conversion reversals use each original's date. The generated transfer uses the selected date (outgoing original by default). Undo reverses the generated transfer and creates ordinary restoration journals on the original dates, copying the original annotations. It works after an account is archived. Explicit restoration links allow provider revisions to follow a chain of undone/reconverted originals to the current journal. Projection rebuilds use all immutable postings; live analytics use surviving ordinary journals.

## Concurrency and ownership

Conversion operations acquire a tenant-scoped transaction advisory lock before deterministically locking source journals and affected accounts. Competing reversal, replacement, classification, reclassification, transfer and import operations acquire the same scope where they can change conversion ownership. A durable journal claim blocks generic mutation of sources and generated machinery; restorations are ordinary journals that can participate in later workflows.

The Ledger persistence contract `ledger.claim_workflow_journal` is installed on Recurring allocations and Sharing source/settlement references, including queued work. Existing associations are backfilled into Ledger's claim table. Recurring unmatch releases claims after queued category compensation reaches a terminal state; completed bill revisions, bill cancellation and settlement reversal release their claims. Ledger's final reclassification check also rejects conversion-managed sources, so stale work cannot allocate a converted transaction. Loans and control-account journals fail ordinary single-user-movement eligibility.

Financial mutations are receipt-backed and payload-sensitive. Conversion state, all journals, projections, audit, outbox and receipts commit together. Failed transactions roll back all effects. An uncertain successful commit can be retried with the same key. Stale previews and expected versions require review.

## Provider imports

An exact signed amount and currency within seven days of a created side is held in `ledger.conversion_import_reviews`. No additional movement is posted while review is pending. Review stores provider-neutral references; the provider event remains in Banking. Multiple matches require selection.

Confirming creates an imported original and its exact neutralizing reversal inside the attachment transaction, preserving auditable evidence without a net balance effect. Dismissing creates the ordinary import. Explicit attachment of an already-posted import reverses its duplicate movement and adds that original to the conversion. Undo restores all attached originals; pending imports resume as ordinary journals in the same unit of work.

Before a provider correction/cancellation, the integration process resolves active restorations and undoes any active conversion. The revision then uses the ordinary Banking correction workflow. Retries resume through existing import receipts and process fencing. Metadata-only revisions remain provider evidence and do not undo a conversion or post another financial journal. Automatically undone conversions appear in the notification query and Android transaction area.

## API and history

`static/openapi.json` documents preview, candidates, conversion, title/note editing, undo, attachment candidates and attachment, review list/detail/resolve, and bank-revision notifications. Financial endpoints require `Idempotency-Key`; metadata edits require an expected version.

`grouped_transfers=true` is opt-in on transaction lists, summaries, and account activity. The database excludes conversion source/reversal machinery before counting, sorting and pagination. A live transfer occupies one row; after undo its ordinary restorations occupy their original dates. Default queries retain the existing audit representation. Rows expose `transfer_conversion_id` when they belong to conversion machinery.

## Android rollout

Android changes are in `/Users/volodymyr/AndroidStudioProjects/MoneyKeeper`. `TRANSFER_CONVERSIONS_ENABLED` defaults to false. Build with `-PtransferConversionsEnabled=true` only after deploying backend migration 0020 and verifying the new endpoints. This avoids exposing financial actions against an older backend. No historical transactions are converted automatically.

The app supports account/counterpart selection, all-date search, missing-side amounts, explicit fees, a date picker, preview, conflict recovery, transfer metadata, original-record navigation, undo, posted-import attachment, pending bank reviews and revision notifications. Requests require connectivity and failed requests preserve the draft and command key. `AppDataChanges` refreshes balances, history and analytics after mutations.

## Verification

Focused Rust tests cover linked fees, both starting directions, missing-side imports, confirmation, undo, tenant isolation, stale preview, concurrent retry, FX with target-currency fees on a liability account, grouped counts, ordinary restoration, projection rebuild and rollback after injected storage failure.

Local verification completed on 2026-09-13:

- Full `cargo test --offline`: 324 passed. Subsequent provider-resolution and queued-claim refinements passed the relevant Banking, Ledger API, migration, Recurring, Sharing and nine conversion integration tests.
- `cargo fmt --check` and `cargo clippy --offline --all-targets --all-features -- -D warnings` passed.
- Android: 162 unit tests passed; debug app and instrumentation APKs built with conversions enabled. Conversion preview/double-submit and Activity UI tests passed individually on the API 36.1 emulator.
- The broader Android UI run exposed an unrelated Dashboard test Hilt setup failure and a manual TalkBack review test requiring opt-in. Those remain outside this implementation; the full UI suite is not green.

## Deployment — 2026-09-13

Backend deployed to `https://moneykeeper.fly.dev` as Fly release 33 using the tested local workspace (base commit `4b1d62f713f781ee2c850958b36c9d9e8fe7bce0` plus uncommitted conversion changes). Image: `registry.fly.io/moneykeeper@sha256:72166a1aac4ada8b763598de7731440ec0788df23a3cd05e3f4a22ab872815ad`.

Fly rolling deployment, machine smoke checks and DNS verification passed. Startup reached database readiness after the embedded migrator and all 12 workers started. Public `/health/live` and `/health/ready` returned 200; conversion review, notification and candidate endpoints returned the expected 401 without authentication. No authenticated production financial mutation was used as a smoke test.

The Android debug APK targets the deployed backend and has `TRANSFER_CONVERSIONS_ENABLED=true`. It was installed with app data preserved and launched on the Samsung SM-F966B (SDK 36) through wireless ADB on 2026-09-13. Installation, activity launch and process verification passed; this was not a functional financial UI test. The Gradle default remains false, so subsequent enabled builds must retain `-PtransferConversionsEnabled=true` (or the equivalent Gradle project property).
