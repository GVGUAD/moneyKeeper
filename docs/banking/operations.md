# Banking and Monobank operations

This guide describes the current Moneykeeper implementation. Start with the [domain context](domain-context.md) and [Banking canvas](bounded-context-banking.md) for ownership and invariants. The [Ledger guide](../ledger.md) covers full server setup, Supabase authentication, account reads, and reconciliation commands; the [reconnection guide](../operations/integration-reconnection.md) covers the development-generation cutover.

## Runtime configuration

Banking runs inside the complete Moneykeeper binary. Startup validates all application configuration and applies embedded migrations through the guarded database initializer. Use an empty dedicated database or one with the `finance-v2` lineage marker; do not manually apply isolated Banking migrations to an unrelated database. Banking storage begins in migration `0004`; `0013` adds Monobank worker persistence, with later migrations extending the integration.

| Variable | Requirement / purpose |
|---|---|
| `DATABASE_URL` | Dedicated PostgreSQL database used by the application |
| `SUPABASE_URL` | Supabase authentication/JWKS base URL |
| `MONOBANK_WEBHOOK_BASE_URL` | Reachable absolute HTTPS callback base with a host and no credentials, query, or fragment |
| `FINANCE_V2_ENCRYPTION_KEY_ID` | Trimmed, non-empty key identifier, at most 100 UTF-8 bytes, without control characters |
| `FINANCE_V2_ENCRYPTION_KEY` | Standard-base64 encoded 32-byte encryption key |
| `FINANCE_V2_WEBHOOK_DIGEST_KEY` | Separate standard-base64 encoded 32-byte webhook digest key |

Retain the configured keys across restarts so persisted credentials and receipts remain usable. Replacing environment keys is not a data re-encryption procedure. Other application requirements, including Gmail configuration, are listed in the Ledger guide.

`banking-sync` is the supervised runtime worker. It performs bounded validation, webhook registration, receipt normalization, statement fetching, provider import, observation delivery, and page-finalization steps. Read `/health/live` and `/health/ready` before exercising business routes; business traffic receives `503 {"error":"not_ready"}` until readiness permits it.

## HTTP conventions

Routes are unversioned. All routes below except the provider callback require `Authorization: Bearer <Supabase access token>` and are tenant-scoped. Mutation handlers require `Idempotency-Key`: non-empty, no surrounding whitespace/control characters, and at most 200 UTF-8 bytes. Keep a key stable for retries of the same intent; use a new key for a new intent.

Retry behavior is operation-specific. Connection creation, credential replacement, and sync requests persist request-hashed receipts. Sync retries return the stored job response; poll the job for current state. Mapping workflows combine receipts or durable mapping identities with version checks; an existing-account retry may hit the resource-version check before reaching its stored receipt. Disconnect and webhook rotation only validate the header and do not persist replay receipts. After an uncertain result, re-read connection/resources before issuing another versioned mutation.

Connection mutations use the connection's `version`. Mapping mutations use the external resource's `version`, not `mapping_version`. Workers can change versions, so re-read immediately before a mutation.

Banking domain errors currently map to `409` for idempotency/version conflicts or an already-active mapping, `404` for invalid state or inactive/missing mapping, and `400` for other Banking rejections. Authentication failures return `401`. OpenAPI lists success responses but does not enumerate every Banking handler error; these descriptions follow the handlers.

Examples use `curl` and `jq`. Set `API` to your server, `TOKEN` to a Supabase access token, and `MONOBANK_TOKEN` to the provider token through your normal secret-handling process. Do not enable shell tracing. Example keys identify one intent and should be replaced for subsequent independent actions.

```sh
export API=http://127.0.0.1:8080
```

## Connect and inspect resources

```sh
CONNECTION_RESPONSE="$(jq -n --arg token "$MONOBANK_TOKEN" '{x_token:$token}' |
  curl -fsS -X POST "$API/provider-connections/monobank" \
    -H "Authorization: Bearer $TOKEN" \
    -H 'Idempotency-Key: monobank-connect-20260908' \
    -H 'Content-Type: application/json' --data-binary @-)"
CONNECTION_ID="$(printf '%s' "$CONNECTION_RESPONSE" | jq -r '.connection.id')"

curl -fsS "$API/provider-connections/$CONNECTION_ID" \
  -H "Authorization: Bearer $TOKEN" | jq
```

Creation returns `202` with `{connection, replayed}`. The connection initially has `state: pending`; provider I/O happens in the worker. Healthy completion has `state: active`, `validation_state: succeeded`, and `webhook_registration_state: registered`, with registered and desired webhook versions equal. `webhook_configured` alone only indicates stored callback configuration.

```sh
curl -fsS "$API/provider-connections/$CONNECTION_ID/resources" \
  -H "Authorization: Bearer $TOKEN" | jq
```

Review each resource's `id`, `provider_resource_id`, `kind`, `funding_model`, `currency`, `discovery_state`, `version`, and `current_mapping`. Use the internal `id` as `RESOURCE_ID` in commands. `latest_provider_balance` and `balance_observed_at` describe provider evidence, not the Ledger balance.

The adapter recognizes Monobank card products `black`, `white`, `platinum`, `iron`, `eAid`, and `yellow`, `fop` current accounts, and jars. Cards with a positive credit limit are classified as revolving credit. Unknown products and resources in known but disabled currencies are discovered as unsupported. Unknown numeric currencies fail normalization. Original transaction-currency evidence can retain a known disabled currency while the accounting effect uses the supported resource currency.

## Map a resource

Choose a resource from the list and set `RESOURCE_ID` and its latest `RESOURCE_VERSION`. To create a provider-observed Ledger account and map it:

```sh
jq -n --arg resource "$RESOURCE_ID" --argjson version "$RESOURCE_VERSION" \
  '{resource_id:$resource,account_name:"Monobank daily card",expected_version:$version}' |
  curl -fsS -X POST "$API/provider-connections/$CONNECTION_ID/resource-mappings" \
    -H "Authorization: Bearer $TOKEN" \
    -H 'Idempotency-Key: monobank-map-card-20260908' \
    -H 'Content-Type: application/json' --data-binary @- | jq
```

For an existing account, send `ledger_account_id` instead of `account_name`. If both are supplied, the existing-account branch wins. Ledger validates tenant, currency, lifecycle, kind, and nature. Own-funds cards require `debit_card`/`asset`, current accounts `current`/`asset`, jars `jar`/`asset`, and revolving-credit cards `credit_card`/`liability`.

The route returns `202` with `{mapping, replayed}`. Creation persists a `pending_account_creation` intent before calling Ledger, then completes the mapping; a retry resumes that identity rather than creating another account. Re-read the resource list to confirm `current_mapping.state: active` and its `ledger_account_id`. Account creation does not seed its balance from the provider snapshot.

To deactivate, call `POST /provider-connections/{id}/resource-mappings/{mapping_id}/deactivations` with `expected_version` and a non-empty printable `reason` of at most 500 bytes. It returns `200` with the mapping result. To replace, use the sibling `/replacements` route and add either `ledger_account_id` or `account_name`; it returns `202`.

Replacement deactivates first and then binds/creates. A failure in the second step can leave the resource unmapped. Re-read resources and resume the intended mapping using current state. Mapping history and prior journals remain intact; changing a mapping does not move previously posted history to another account.

## Request and monitor synchronization

Each new HTTP sync request requires one supported, actively mapped resource on an active connection. The interval must not be inverted. `overlap_seconds` defaults to zero, accepts 0–86400, and extends the fetch start backward from `requested_from`.

```sh
SYNC_RESPONSE="$(jq -n --arg resource "$RESOURCE_ID" \
  '{resource_id:$resource,requested_from:"2026-08-01T00:00:00Z",requested_to:"2026-08-31T23:59:59Z",overlap_seconds:0}' |
  curl -fsS -X POST "$API/provider-connections/$CONNECTION_ID/sync-jobs" \
    -H "Authorization: Bearer $TOKEN" \
    -H 'Idempotency-Key: monobank-card-august-2026' \
    -H 'Content-Type: application/json' --data-binary @-)"
SYNC_JOB_ID="$(printf '%s' "$SYNC_RESPONSE" | jq -r '.id')"

curl -fsS "$API/sync-jobs/$SYNC_JOB_ID" \
  -H "Authorization: Bearer $TOKEN" | jq
curl -fsS "$API/sync-jobs/$SYNC_JOB_ID/pages" \
  -H "Authorization: Bearer $TOKEN" | jq
```

The request returns `202` with the job directly, without a `job` wrapper. The worker fetches oldest-first windows of up to 30 days and reserves at least 61 seconds between statement requests on a connection. These are implementation scheduling values, not a claim about the provider's current external API limits.

Pages retain window boundaries, event links, and processed/quarantined counts. Progress advances only when every linked event is `posted`, `no_financial_change`, or `quarantined`. Missing mappings, waiting revisions, retrying imports, and even `terminal_failure` processes can leave a page waiting. A completed job may still include quarantined evidence and unresolved reconciliation cases.

The job captures connection version and credential generation. If these no longer match after a connection mutation, old work cannot be claimed or completed normally. After credential/webhook recovery, inspect old jobs and request a new job with a new key for the intended range when necessary; do not assume a repeated original request creates fresh work.

## Webhooks and credential recovery

The worker generates and registers the callback automatically. The provider uses `GET` and `POST /webhooks/monobank/{webhook_credential}`; this route authenticates by the path secret rather than a Supabase bearer token. A valid check or accepted durable receipt returns `200`; handler failures return `404`. A `200` receipt does not mean normalization or Ledger import has completed. Identical callback deliveries are deduplicated before downstream processing.

Validation, registration, and statement workers use 30-second leases. Transient and rate-limited provider failures retry with exponential delays starting at 61 seconds and capped at one hour, incorporating a bounded provider retry delay. Retry scheduling stops once the claim attempt count reaches ten. Credential and terminal failures are not automatically retried by that policy.

For validation inspect `validation_state`, `validation_attempts`, `validation_next_retry_at`, and `validation_last_error_class`; webhook registration has corresponding fields. For a job inspect `state`, `attempts`, `next_retry_at`, and `last_error`. Wait until scheduled retry times rather than submitting duplicate work.

For `needs_reauth` or failed credential validation, fetch the latest connection version into `CONNECTION_VERSION` and submit a replacement token:

```sh
jq -n --arg token "$MONOBANK_TOKEN" --argjson version "$CONNECTION_VERSION" \
  '{x_token:$token,expected_version:$version}' |
  curl -fsS -X POST "$API/provider-connections/$CONNECTION_ID/credential-replacements" \
    -H "Authorization: Bearer $TOKEN" \
    -H 'Idempotency-Key: monobank-token-replacement-20260908' \
    -H 'Content-Type: application/json' --data-binary @- | jq
```

The `202` result contains `{connection, replayed}`. Poll validation and webhook registration again. A replacement is a candidate until successful validation; failed validation can leave the prior credential retained. Check both lifecycle and validation status instead of assuming `active` means the new token succeeded.

To rotate a webhook secret, post `{"expected_version":<latest connection version>}` to `/provider-connections/{id}/webhook-rotations` with authentication and an idempotency header. The `201` response exposes only `connection_id`, `desired_version`, and `connection_version`. Rotation invalidates the old lookup secret immediately, and the worker registers the new callback; poll until registration catches up. There is no public callback-secret read endpoint.

Disconnect uses the same version-only body at `/provider-connections/{id}/disconnect` and returns the connection directly with `200`. It marks the connection `revoked`, clears stored provider/callback credentials and lookup digest, disables validation/registration work, and cancels unfinished sync jobs. It preserves resources, evidence, and Ledger history. It does not issue a remote Monobank webhook-unregistration call, nor does it promise to erase or halt all already-ingested accounting evidence.

## Imports, observations, and diagnostics

Statement and webhook normalization use signed account-currency money for Ledger effects and retain original money as evidence. The current import policy can post pending provider revisions. An unchanged settled revision with the same money and description reuses the previous journal; monetary corrections reverse the prior journal and import a new one. Provider reversal commands append a reversal rather than deleting history.

Comparable own-funds observations are delivered to Ledger reconciliation. Revolving-credit balances remain non-comparable because their semantics require review. Observation delivery can record `ignored_older`; a recent receipt is not necessarily a newer provider observation. Use the [Ledger reconciliation workflow](../ledger.md#reconciliation) to inspect, approve, or dismiss cases. Import completion and observation delivery alone do not approve a balance adjustment.

| Method | Path | Purpose |
|---|---|---|
| `GET` | `/provider-connections` | List tenant connections |
| `GET` | `/provider-connections/{id}` | Inspect lifecycle, versions, validation, registration |
| `GET` | `/provider-connections/{id}/resources` | Inspect discovery, current mapping, latest provider balance |
| `GET` | `/provider-connections/{id}/provider-event-conflicts` | Inspect conflict IDs, affected event IDs, reasons, and times |
| `GET` | `/sync-jobs/{id}` | Inspect range, progress, captured versions, retry state |
| `GET` | `/sync-jobs/{id}/pages` | Inspect windows and processed/quarantined counts |
| `GET` | `/provider-events/{id}` | Inspect normalized revision and its processing status/error |
| `GET` | `/accounting-processes/{id}` | Inspect a known durable process ID |
| `GET` | `/balance-observations/{id}` | Inspect basis, comparability, and delivery links |

These reads return `200` on success. There is no public provider-event conflict-resolution or arbitrary event-retry mutation route. Diagnose the underlying mapping, provider evidence, or worker failure before attempting recovery; do not modify immutable facts to make a status appear healthy.

Record only internal operational IDs and redacted status/error classes in diagnostics. Never log provider tokens, callback URLs, raw request/response bodies, financial amounts/descriptions, merchant evidence, resource labels, or encryption envelopes. HTTP responses carry `x-request-id`; use it and durable correlation IDs to correlate failures. Tenant API views can contain financial evidence and should not be copied wholesale into logs or tickets.

## Verification sources

Existing tests document the scenarios behind this guide: `banking_domain`, `banking_monobank`, and `banking_persistence` cover model, normalization, encryption, and storage rules; `banking_api` and `banking_ledger_contract` cover tenant/resource scoping and account binding; `banking_sync`, `banking_webhook`, `banking_worker`, and `banking_workflow` cover deduplication, fencing, recovery, revision imports, and observations. The [OpenAPI contract](../../static/openapi.json) provides request and response schemas. Documentation changes do not require live provider calls.
