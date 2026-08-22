# Finance V2 integration reconnection

Finance V2 is a clean development generation. It does not import access
tokens, refresh tokens, webhook credentials, provider cursors, Gmail history
IDs, or OAuth state from the legacy database. Reconnection is deliberate and
must happen only after the V2 marker, workers, and authenticated API smoke tests
are healthy.

## Monobank

1. Start a new connection through the authenticated V2 API and submit the
   provider token once. Do not copy encrypted legacy credential rows.
2. Fetch the provider account list. Review every discovered card, current
   account, and jar. Confirm its provider resource ID, product kind, native
   currency, displayed balance, and whether it represents an asset or
   liability before mapping it to a Ledger account.
3. Create or select the correct Ledger account, then map each provider resource
   explicitly. Never infer a securities account from an unfamiliar resource
   and never use provider balance as a direct balance setter.
4. Rotate the webhook credential, build the callback from the approved HTTPS
   base, and register/test it. The full callback URL contains a secret and must
   not appear in tickets, logs, screenshots, or this record.
5. Request synchronization. Review provider-event conflicts, failed pages, and
   reconciliation cases. Approve, dismiss, or correct cases deliberately before
   treating balances and reports as authoritative.

Record only connection IDs, resource kinds, currencies, mapping decisions,
sync timestamps, and redacted success/failure status. Never record tokens,
webhook path credentials, raw responses, or encrypted envelopes.

## Gmail

1. Start a new OAuth authorization from Finance V2. The redirect URI must match
   the provider console and the candidate configuration exactly.
2. Complete consent for the intended mailbox. Do not copy OAuth credentials,
   cursor/history state, or encrypted message payloads from the legacy schema.
3. Request a sync and wait for its durable job to complete. Confirm the cursor
   advances only after complete pages and that credential/sync generations have
   not fenced the worker.
4. Review ignored messages, retryable failures, immutable source-message
   revisions, parser attempts, and receipt evidence. Resolve failed evidence or
   Recurring matching cases before relying on subscription or report views.

## Operational completion and rollback

Reconnection is complete only when Monobank resources are mapped, the secret
webhook succeeds, Gmail sync is current, reconciliation/provider failures are
operator-visible, and reports rebuild to the same totals as their source facts.

Rollback does not migrate any V2-created connection or financial data. Stop V2
readiness and workers, restore the prior binary and its prior secret
`DATABASE_URL`, verify the untouched legacy database/volume, and start the
legacy process. Preserve both database names, volumes, and backups until the
development owner approves a later manual cleanup outside the Phase 8 plan.
