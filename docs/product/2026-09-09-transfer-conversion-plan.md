# Convert an existing transaction to a transfer

Status: Implemented and locally verified. Backend deployed as Fly release 33 on 2026-09-13; enabled Android APK installed and launched on the Samsung SM-F966B.
Implementation notes: [Transfer conversion domain and rollout](../transfer-conversions.md).
Date: 2026-09-09.

## Summary

Add the complete feature to the Rust backend and Android app at `/Users/volodymyr/AndroidStudioProjects/MoneyKeeper`.

From transaction details, users can convert a manual or bank-imported income/expense into a transfer by linking another transaction or creating its missing side. Preserve original records, show a preview, and support undo.

## Phone experience

- Add **Convert to transfer** to transaction details. Show an explanation when the transaction is ineligible.
- Select another active account belonging to the user. Infer outgoing/incoming direction from the starting transaction’s account posting.
- Show suggested opposite-side transactions, plus search and **Create missing side**. Default suggestions to transactions within seven days; prioritize equal amounts for the same currency. Search can find eligible transactions outside that window.
- Preserve existing transaction amounts. For a missing side in another currency, request its amount. Support an explicit fee in either represented currency.
- For same-currency outgoing/incoming amounts such as 102 and 100, require confirmation that 2 is a fee. Reject incoming amounts greater than outgoing amounts.
- Use one editable transfer date for both sides, defaulting to the outgoing transaction’s date, or the starting transaction’s date when creating the outgoing side. Preserve original dates in source details.
- Preview accounts, amounts, fee, date, balance changes, and removal from income/expense totals. Confirmation submits one operation.
- Show one transfer row in the main list and the relevant movement in each account’s history. Details include an editable title/note, original records, and **Undo conversion**.
- Refresh balances, history, and analytics after conversion, linking, or undo. Financial actions require connectivity; retain form input after failures.

## Backend and accounting

- Extend the existing Ledger model with a versioned `TransferConversion` record: source journal references, generated journal references, original metadata, selected date, transfer metadata, and lifecycle history. Banking retains ownership of provider evidence.
- Conversion reverses eligible originals and posts the transfer within the existing Ledger unit of work. Persist conversion state, balance projections, idempotency receipt, and outbox events atomically.
- Linking two originals must leave current account balances unchanged. Creating one side must preserve the existing side’s balance effect and apply only the missing movement. Fees remain expenses; transfer principal contributes no income or expense.
- Reuse transfer posting rules, exact decimal money, and FX clearing accounts. Derive the displayed source-per-target exchange rate from principal amounts; never recalculate recorded amounts from a rounded rate.
- Undo reverses the generated transfer and creates ordinary restoration journals with the originals’ amounts, dates, and metadata. Keep explicit restoration links: merely reversing a reversal would not restore the current analytics correctly.
- Resolve imported transaction references to their active restored journal after undo, so subsequent bank corrections operate on the correct record.
- Allow only live manual/imported ordinary income or expense journals with one user-account movement. Reject reversed, replaced, already-converted, or financially allocated transactions. Resolve shared-bill, loan, recurring, and other workflow associations first.
- Enforce conversion claims through Ledger contracts for both conversion and competing workflows, including work queued before conversion. Lock affected records in deterministic order and revalidate at commit.
- Block generic replacement/reversal of conversion-managed journals; expose whole-conversion undo instead. Permit undo after an account is archived.

## APIs, history, and bank sync

Add these endpoints:

| Method | Route | Purpose |
| --- | --- | --- |
| `GET` | `/transactions/{id}/transfer-candidates` | Suggest and search eligible counterparts. |
| `POST` | `/transactions/{id}/transfer-conversion-preview` | Validate and preview a conversion. |
| `POST` | `/transactions/{id}/transfer-conversions` | Commit a conversion. |
| `GET` | `/transfer-conversions/{id}` | Read transfer details, source evidence, and lifecycle. |
| `PATCH` | `/transfer-conversions/{id}` | Edit transfer title and note. |
| `POST` | `/transfer-conversions/{id}/undo` | Undo the entire conversion. |

- Conversion inputs identify the other account and either an existing journal or missing-side amount, plus fee, date, title, and note. Preview returns normalized amounts, balance effects, eligibility issues, and a version token. Financial mutations require an idempotency key; stale previews return a conflict requiring review.
- Add an opt-in grouped history representation to transaction/account queries, with matching summary counts and server-side pagination. Preserve existing default API responses. Group originals and correction machinery under the transfer while retaining direct audit access.
- Before posting a new bank transaction, check for an active conversion with a created side in that account. An exact currency/amount match within seven days becomes **pending review**, without another balance posting. Multiple matches require explicit selection.
- Add review list/detail and confirm/dismiss actions. Surface them in the app’s transaction area and transfer details. **Not a match** posts the import normally; confirmation attaches its evidence without adding another movement.
- Also support explicitly linking an already-posted eligible import: neutralize its duplicate accounting effect and attach it atomically.
- If a bank changes an attached amount or cancels the transaction, automatically undo the conversion, apply the provider revision, and create a visible notification. Metadata-only revisions update evidence without undo.
- Make import, review, conversion, and undo races recoverable and idempotent. Undo restores all attached originals; a still-pending import resumes normal import processing.
- Add migrations, OpenAPI contracts, Android DTO/repository support, and focused domain documentation under `docs/`. Deploy backend support before exposing the Android action; do not automatically convert historical transactions.

## Validation

- Test both starting directions, one/two originals, manual/imported combinations, same-currency fees, FX, different original dates, and asset/liability accounts.
- Verify exact balances, historical date effects, fee reporting, exclusion of principal from analytics, and restoration after undo—including projection rebuilds.
- Test tenant isolation, archived accounts, workflow claims, stale previews, repeated requests, concurrent conversions, and rollback on partial failure.
- Cover pending import confirmation/dismissal, ambiguous matches, already-posted duplicates, provider corrections/cancellations, retries, and races with undo.
- Test Android selection/search, amount and fee validation, preview, conflict recovery, grouped pagination, original-detail navigation, pending reviews, and undo.
- Run the relevant Rust integration/concurrency/reporting suites and Android unit/UI tests, then build the Android app.
