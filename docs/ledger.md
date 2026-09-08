# Ledger: how it works, how to configure it, and how to use it

This guide describes the Ledger implemented in the current Moneykeeper source code. The source code remains authoritative when behavior changes.

## What the Ledger is

The Ledger is Moneykeeper's tenant-scoped, immutable, double-entry accounting core. An authenticated user's UUID is the tenant boundary. Every account, journal, posting, annotation, command receipt, and reconciliation case is read or written within that boundary.

Clients do not send arbitrary debit and credit lines. They send a financial intent—open an account, record income or an expense, transfer funds, correct a balance, reverse an entry, or approve a reconciliation—and the Ledger builds the balanced postings.

The important model is:

- An **account** holds identity and metadata, not its balance.
- A **journal entry** is an immutable accounting fact.
- A **posting** is one immutable account effect owned by a journal.
- An **account balance** is a projection calculated by applying postings.
- A **transaction annotation** is mutable, versioned metadata kept separate from the immutable journal.
- A **reconciliation case** compares a provider observation with a captured Ledger balance and records the user's decision.

Financial history is never edited or deleted. Mistakes are handled with new correction, reversal, or replacement journals.

## Accounting behavior

### Signed balances and display balances

Internally, posting amounts are debit-positive and credit-negative. API responses expose both the raw `signed_balance` and the normalized `display_balance`.

| Account nature | Normal sign | Display balance |
| --- | ---: | --- |
| `asset` | `+1` | same as signed balance |
| `expense` | `+1` | same as signed balance |
| `liability` | `-1` | negated signed balance |
| `equity` | `-1` | negated signed balance |
| `income` | `-1` | negated signed balance |

User-facing code should normally display `display_balance`. For example, a credit card with `signed_balance: "-80.00"` has `display_balance: "80.00"`, meaning 80 is owed.

The same distinction appears on postings and command results:

- `signed_amount` is the raw debit-positive/credit-negative posting amount.
- `display_effect` is the effect after applying the account nature's normal sign.

### Account types

The HTTP API opens manual, user-visible accounts. Only these kind/nature combinations are accepted:

| `kind` | Required `nature` | Typical use |
| --- | --- | --- |
| `cash` | `asset` | Physical cash |
| `debit_card` | `asset` | Debit card balance |
| `current` | `asset` | Current/checking account |
| `savings` | `asset` | Savings account |
| `jar` | `asset` | Savings jar or envelope |
| `loan_receivable` | `asset` | Money owed to the user |
| `credit_card` | `liability` | Credit card debt |
| `loan_payable` | `liability` | Money the user owes |

`income`, `expense`, and `equity` accounts are hidden system accounts. They are created on demand by Ledger recipes and cannot be opened through `POST /accounts`. Provider-observed accounts are created through the Banking integration rather than through the public Ledger endpoint.

An account's currency, nature, and kind are effectively fixed after creation. The public API supports renaming, archiving, and restoring it.

### How each command posts

The Ledger applies these recipes:

| Intent | User-account posting | Counter-posting |
| --- | --- | --- |
| Opening balance | `opening_balance * normal_sign` | Opposite amount to hidden opening-balance equity |
| Income | `+amount` | `-amount` to hidden uncategorized income |
| Expense | `-amount` | `+amount` to hidden uncategorized expense |
| Same-currency transfer | `-amount` on source, `+amount` on target | No separate clearing account |
| Cross-currency transfer | `-source_amount` and `+target_amount` | Opposite lines in one hidden FX-clearing account per currency |
| Transfer fee | `-fee` on the selected user account | `+fee` to hidden uncategorized expense |
| Balance correction | Exact signed delta needed to reach the target display balance | Opposite amount to hidden balance-adjustment equity |
| Approved reconciliation | Captured provider/Ledger display delta | Opposite amount to hidden balance-adjustment equity |
| Reversal | Exact negation of every original posting | The whole original journal is negated |

Every journal has at least two non-zero postings, belongs to one user, and balances to zero independently in every currency. These rules are checked in the domain and again by PostgreSQL constraints.

For a cross-currency transfer, the caller supplies the exact source amount, target amount, and a positive `implied_rate`. The implementation records the rate but does not calculate an amount from it or verify that the amounts mathematically match it.

### Immutability and versions

Journal entries, postings, correction details, and audit events cannot be updated or deleted. PostgreSQL triggers enforce this in addition to the application design.

There are three independent optimistic-concurrency versions:

- `account.version` protects account metadata and lifecycle changes.
- `account.balance_version` protects balance corrections and reconciliation approval.
- `annotation.version` protects transaction metadata changes.

Always use the latest version returned by a read or mutation. Do not assume a version increases by exactly one per business command: a journal may apply more than one posting to the same account, such as a same-currency transfer fee charged to its source.

## Runtime configuration

The Ledger has no Ledger-specific environment variables. It runs inside the complete Moneykeeper binary, whose startup validates all application configuration before connecting to external providers.

### Required server variables

| Variable | Requirement |
| --- | --- |
| `DATABASE_URL` | PostgreSQL connection URL. The database must be empty or already carry the `finance-v2` Moneykeeper lineage marker. |
| `SUPABASE_URL` | Absolute HTTP or HTTPS Supabase base URL. Startup fetches `auth/v1/.well-known/jwks.json` from it. |
| `MONOBANK_WEBHOOK_BASE_URL` | Absolute HTTPS URL with a host and no credentials, query, or fragment. |
| `FINANCE_V2_ENCRYPTION_KEY_ID` | Non-empty, trimmed identifier with no control characters and at most 100 UTF-8 bytes. |
| `FINANCE_V2_ENCRYPTION_KEY` | Standard-base64 value that decodes to exactly 32 bytes. |
| `FINANCE_V2_WEBHOOK_DIGEST_KEY` | Standard-base64 value that decodes to exactly 32 bytes. |
| `GMAIL_CLIENT_ID` | Non-empty. |
| `GMAIL_CLIENT_SECRET` | Non-empty. |
| `GMAIL_REDIRECT_URI` | Non-empty. |

Optional variables:

| Variable | Default | Purpose |
| --- | --- | --- |
| `BIND_ADDR` | `0.0.0.0:8080` | HTTP listener socket address |
| `RUST_LOG` | `moneykeeper=info` | Log filtering, for example `moneykeeper=debug` for temporary local diagnosis |
| `LOG_FORMAT` | `compact` | `compact` for readable terminal output or `json` for structured aggregation; other values fail startup |

Every HTTP response includes an `x-request-id`. A client-supplied identifier is
accepted only when it contains 1–64 ASCII letters, digits, `.`, `_`, or `-`;
otherwise the server generates a UUID. Command logs also carry the durable
correlation ID used by background workflows.

Logs intentionally contain route templates and internal operational UUIDs, not
raw request targets. Do not add request/response bodies, headers, access tokens,
OAuth state or codes, webhook callback URLs, provider identifiers, financial
amounts, descriptions, or merchant data to tracing events. SQL/body/header
tracing must remain disabled in environments with real data.

`PUBLIC_URL` is present in the deployment file but is not read by the current Rust runtime.

Generate the two 32-byte keys separately and paste their outputs into `.env`:

```bash
openssl rand -base64 32
openssl rand -base64 32
```

The binary loads `.env` from its working directory when present. A local configuration has this shape:

```dotenv
DATABASE_URL=postgres://postgres:postgres@127.0.0.1:5432/moneykeeper_v2
BIND_ADDR=127.0.0.1:8080
SUPABASE_URL=https://your-project.supabase.co/

MONOBANK_WEBHOOK_BASE_URL=https://your-public-callback.example/
FINANCE_V2_ENCRYPTION_KEY_ID=local-development-key
FINANCE_V2_ENCRYPTION_KEY=<base64-encoded-32-byte-key>
FINANCE_V2_WEBHOOK_DIGEST_KEY=<different-base64-encoded-32-byte-key>

GMAIL_CLIENT_ID=<client-id>
GMAIL_CLIENT_SECRET=<client-secret>
GMAIL_REDIRECT_URI=http://127.0.0.1:8080/oauth/gmail/callback
```

The Monobank callback must be a real reachable HTTPS base when Banking is used. If no provider connection exists, a syntactically valid placeholder can satisfy startup for Ledger-only local work, but it must not be used for an active Banking connection. Gmail values likewise need working credentials only when the Gmail integration is used, although startup still requires them to be non-empty.

### Database initialization

Start the repository's PostgreSQL 16 service:

```bash
docker compose up -d postgres_v2
```

Then start Moneykeeper:

```bash
cargo run
```

Startup automatically runs the embedded migrations. It intentionally refuses to migrate a non-empty database that does not have the Moneykeeper lineage marker. Use a dedicated `moneykeeper_v2` database rather than pointing the process at an unrelated or legacy database.

Readiness endpoints do not require authentication:

```bash
curl -sS http://127.0.0.1:8080/health/live
curl -sS http://127.0.0.1:8080/health/ready
```

Business routes return `503 {"error":"not_ready"}` until the worker registry has started.

### Authentication

All Ledger routes require:

```http
Authorization: Bearer <Supabase access token>
```

The token must be ES256-signed by a key in the configured Supabase JWKS, have audience `authenticated`, and have a UUID in `sub`. That `sub` UUID becomes the Ledger tenant ID. A request for another tenant's resource is returned as `404`, not as an authorization disclosure.

For local testing, `scripts/get_token.sh` reads `SUPABASE_URL`, `SUPABASE_ANON_KEY`, `TEST_EMAIL`, and `TEST_PASSWORD` from `.env` and returns the Supabase token response. With `jq` installed:

```bash
export API=http://127.0.0.1:8080
export TOKEN="$(./scripts/get_token.sh | jq -r '.access_token')"
```

`SUPABASE_ANON_KEY` and the test credentials are helper-script inputs; they are not required by the Moneykeeper server itself.

### Currencies and categories

The database seeds three enabled currencies:

- `UAH`, minor unit 2
- `USD`, minor unit 2
- `EUR`, minor unit 2

List the currently enabled values with:

```bash
curl -sS "$API/currencies" \
  -H "Authorization: Bearer $TOKEN" | jq
```

Currency codes must be exactly three uppercase ASCII letters. Money amounts are JSON strings, are exact decimals rather than floating-point numbers, and may not have more fractional digits than the currency's minor unit. The current API exposes currency reads but no currency-management endpoint; adding another currency requires changing reference data through a controlled database migration or administrative process.

Categories are optional for manual income, expenses, and replacements. Create one before using its ID:

```bash
CATEGORY_RESPONSE="$(curl -sS -X POST "$API/categories" \
  -H "Authorization: Bearer $TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"name":"Food","kind":"expense"}')"

export CATEGORY_ID="$(jq -r '.id' <<<"$CATEGORY_RESPONSE")"
```

Category kinds are `income`, `expense`, and `both`. Recording or replacing a transaction requires the category to be active and compatible with the transaction kind. A later annotation edit currently checks that a category is active but does not re-check its kind against the original transaction.

## HTTP conventions

Ledger routes are unversioned. There is no `/v2` alias.

All JSON request DTOs reject unknown fields. All mutation routes require a valid `Idempotency-Key` header:

```http
Idempotency-Key: a-caller-generated-unique-key
```

A key must be non-empty, contain no surrounding whitespace or control characters, and be at most 200 UTF-8 bytes. Keys are scoped by user and command type. Retrying the same command in the same scope with the same command payload returns the original result with `replayed: true`. Monetary decimal strings are normalized before the command is hashed, but do not rely on other semantically equivalent JSON forms hashing identically. Reusing the key in that scope for a different request returns `409`.

Use a new key for every new intent and retain it when retrying after a timeout.

For reliable retries, send an explicit, stable `occurred_at`. Most financial-command hashes include the effective occurrence time. If it is omitted, each HTTP attempt substitutes a new server time and a later attempt with the same idempotency key can therefore conflict even when the submitted JSON is otherwise identical. Rename, archive, and restore do not include `occurred_at` in their request hash, but consistently supplying it remains the simplest client policy.

Money has this shape:

```json
{
  "amount": "12.50",
  "currency": "UAH"
}
```

Timestamps are UTC-capable RFC 3339 values such as `2026-08-24T10:30:00Z`. Most mutation requests make `occurred_at` optional and use the server's current UTC time when it is absent. `observed_at` on a balance correction is required because it describes when the balance was checked.

Typical Ledger error responses are:

| Status | Meaning | Body |
| ---: | --- | --- |
| `400` | Invalid JSON, unknown fields, missing/invalid header, invalid money, or a failed domain rule | `{"error":"invalid ledger request"}` or a more specific adapter error |
| `401` | Missing or invalid bearer token | `{"error":"unauthorized"}` |
| `404` | Missing resource, cross-tenant resource, or removed route | `{"error":"ledger resource was not found"}` or `{"error":"not_found"}` |
| `409` | Stale version, idempotency conflict, archived account, duplicate reversal, or stale reconciliation | `{"error":"ledger conflict"}` |
| `500` | Persistence or internal failure | `{"error":"internal server error"}` |
| `503` | Application has not reached readiness | `{"error":"not_ready"}` |

The public Ledger error contract is intentionally coarse. On `409`, re-read the current resource and compare the appropriate version before deciding whether to retry.

## Configure user accounts

### Open an account

```bash
OPEN_RESPONSE="$(curl -sS -X POST "$API/accounts" \
  -H "Authorization: Bearer $TOKEN" \
  -H 'Content-Type: application/json' \
  -H 'Idempotency-Key: open-main-wallet-20260824' \
  -d '{
    "name": "Main wallet",
    "currency": "UAH",
    "kind": "cash",
    "nature": "asset",
    "opening_balance": "1000.00",
    "occurred_at": "2026-08-24T09:00:00Z"
  }')"

export ACCOUNT_ID="$(jq -r '.account.id' <<<"$OPEN_RESPONSE")"
jq . <<<"$OPEN_RESPONSE"
```

`POST /accounts` returns `201` with:

- `account`, including both balance forms and both versions;
- `opening_journal_id`, which is `null` only when the opening balance is zero;
- `replayed`, which identifies an idempotent replay.

An opening balance may be positive, zero, or negative. A positive liability opening balance is converted to a negative signed balance and a positive display balance.

### List and inspect accounts

```bash
curl -sS "$API/accounts" \
  -H "Authorization: Bearer $TOKEN" | jq

curl -sS "$API/accounts/$ACCOUNT_ID" \
  -H "Authorization: Bearer $TOKEN" | jq
```

`GET /accounts` includes active and archived user-visible accounts, sorted case-insensitively by name. Hidden system accounts are never included.

`GET /accounts/{id}` may additionally populate `provider_reported`, `available`, and `reconciliation_difference` when Banking has a provider summary for the account. Those fields are otherwise `null`. The list endpoint does not enrich each account with those Banking values.

### Rename, archive, and restore

Use `account.version`, not `balance_version`, for these operations:

```bash
curl -sS -X PATCH "$API/accounts/$ACCOUNT_ID" \
  -H "Authorization: Bearer $TOKEN" \
  -H 'Content-Type: application/json' \
  -H 'Idempotency-Key: rename-main-wallet-20260824' \
  -d '{"name":"Daily cash","expected_version":1}' | jq
```

```bash
curl -sS -X POST "$API/accounts/$ACCOUNT_ID/archive" \
  -H "Authorization: Bearer $TOKEN" \
  -H 'Content-Type: application/json' \
  -H 'Idempotency-Key: archive-main-wallet-20260824' \
  -d '{"expected_version":2}' | jq
```

```bash
curl -sS -X POST "$API/accounts/$ACCOUNT_ID/restore" \
  -H "Authorization: Bearer $TOKEN" \
  -H 'Content-Type: application/json' \
  -H 'Idempotency-Key: restore-main-wallet-20260824' \
  -d '{"expected_version":3}' | jq
```

Archiving preserves balance and history. It blocks ordinary openings of new activity against the account: manual income/expenses, transfers, and manual replacements. Explicit repairs—corrections, reversals, and approved reconciliations—remain allowed.

## Record income and expenses

The amount must be positive and its currency must match the selected account.

```bash
TRANSACTION_RESPONSE="$(curl -sS -X POST "$API/transactions" \
  -H "Authorization: Bearer $TOKEN" \
  -H 'Content-Type: application/json' \
  -H 'Idempotency-Key: expense-lunch-20260824' \
  -d "{
    \"account_id\": \"$ACCOUNT_ID\",
    \"kind\": \"expense\",
    \"amount\": {\"amount\": \"125.50\", \"currency\": \"UAH\"},
    \"description\": \"Lunch\",
    \"category_id\": \"$CATEGORY_ID\",
    \"note\": \"Team lunch\",
    \"tags\": [\" Food \", \"work\", \"food\"],
    \"budget_visibility\": \"included\",
    \"occurred_at\": \"2026-08-24T12:15:00Z\"
  }")"

export JOURNAL_ID="$(jq -r '.journal_entry_id' <<<"$TRANSACTION_RESPONSE")"
jq . <<<"$TRANSACTION_RESPONSE"
```

Use `"kind":"income"` for income. `category_id`, `note`, `tags`, `budget_visibility`, and `occurred_at` are optional. Defaults are no category, no note, empty tags, `included`, and the current server time.

Descriptions are trimmed and must contain 1 to 500 characters. Notes are trimmed, an empty note becomes absent, and a non-empty note is limited to 2,000 characters.

Tags are:

- trimmed and Unicode-lowercased;
- deduplicated and sorted;
- limited to 20 unique tags;
- limited to 40 characters per tag;
- rejected when empty after trimming or when they contain control characters.

`budget_visibility` is either `included` or `excluded` and is explicit metadata for downstream budgeting/reporting consumers.

## Read journals and activity

```bash
curl -sS "$API/transactions/$JOURNAL_ID" \
  -H "Authorization: Bearer $TOKEN" | jq

curl -sS "$API/transactions?limit=50" \
  -H "Authorization: Bearer $TOKEN" | jq

curl -sS "$API/accounts/$ACCOUNT_ID/activity?limit=50" \
  -H "Authorization: Bearer $TOKEN" | jq
```

Journal detail includes:

- immutable `description`, `source`, `purpose`, actor, occurrence/recording times, and correlation ID;
- ordered postings with their signed and display effects;
- optional mutable `annotation`;
- forward links through `reversed_by_journal_id` and `replaced_by_journal_id`;
- the journal's own relation fields, such as `reverses_transaction_id`;
- optional balance-correction detail.

`occurred_at` is the business time supplied by the caller. `recorded_at` is when Moneykeeper persisted the fact. `ledger_sequence` is a database-assigned stable tie-breaker.

Lists are sorted by `(occurred_at, ledger_sequence)` descending. `limit` defaults to 50 and must be from 1 through 200. To fetch the next page, copy both values from the last item:

```bash
curl -sS -G "$API/transactions" \
  -H "Authorization: Bearer $TOKEN" \
  --data-urlencode 'after_occurred_at=2026-08-24T12:15:00Z' \
  --data-urlencode 'after_sequence=1234' \
  --data 'limit=50' | jq
```

Providing only one cursor component, or a non-positive sequence, returns `400`.

## Edit transaction metadata

Only manual income/expense journals and manual replacement journals are created with an annotation. Opening-balance, transfer, correction, reconciliation, and reversal journals do not have one, so their annotation route returns `404`.

Editing an annotation does not change its journal or postings. Use `annotation.version`:

```bash
curl -sS -X PATCH "$API/transactions/$JOURNAL_ID/annotation" \
  -H "Authorization: Bearer $TOKEN" \
  -H 'Content-Type: application/json' \
  -H 'Idempotency-Key: annotate-lunch-20260824' \
  -d '{
    "expected_version": 1,
    "description": "Client lunch",
    "note": "Receipt attached",
    "tags": ["client", "food"],
    "budget_visibility": "excluded"
  }' | jq
```

Omitted fields are unchanged. To clear nullable fields:

```json
{
  "expected_version": 2,
  "clear_category": true,
  "clear_note": true
}
```

Do not provide `category_id` together with `clear_category: true`, or `note` together with `clear_note: true`. Set `tags` to `[]` to clear all tags. A patch that produces no actual change is rejected as an invalid request.

The journal's top-level `description` remains the original immutable accounting description. The annotation's `description` is the editable user-facing metadata.

## Transfer funds

### Same currency

Source and target accounts must differ. Both amounts must be positive, their currencies must match the corresponding accounts, and same-currency amounts must be equal. Do not send `implied_rate` for a same-currency transfer.

```bash
curl -sS -X POST "$API/transfers" \
  -H "Authorization: Bearer $TOKEN" \
  -H 'Content-Type: application/json' \
  -H 'Idempotency-Key: transfer-wallet-to-card-20260824' \
  -d "{
    \"source_account_id\": \"$ACCOUNT_ID\",
    \"target_account_id\": \"$TARGET_ACCOUNT_ID\",
    \"source_amount\": {\"amount\": \"200.00\", \"currency\": \"UAH\"},
    \"target_amount\": {\"amount\": \"200.00\", \"currency\": \"UAH\"},
    \"fee\": {\"amount\": \"2.00\", \"currency\": \"UAH\"},
    \"description\": \"Fund debit card\"
  }" | jq
```

For same-currency transfers, a fee is charged to the source account.

### Cross currency

```bash
curl -sS -X POST "$API/transfers" \
  -H "Authorization: Bearer $TOKEN" \
  -H 'Content-Type: application/json' \
  -H 'Idempotency-Key: transfer-uah-to-usd-20260824' \
  -d "{
    \"source_account_id\": \"$UAH_ACCOUNT_ID\",
    \"target_account_id\": \"$USD_ACCOUNT_ID\",
    \"source_amount\": {\"amount\": \"4100.00\", \"currency\": \"UAH\"},
    \"target_amount\": {\"amount\": \"100.00\", \"currency\": \"USD\"},
    \"fee\": {\"amount\": \"1.00\", \"currency\": \"USD\"},
    \"implied_rate\": \"41.00\",
    \"description\": \"Buy USD\"
  }" | jq
```

A cross-currency fee may be in either the source or target currency. It is charged to the user account with that currency. A fee in any third currency is rejected.

There is no insufficient-funds rule in the Ledger command: an asset balance may become negative. If the product needs such a rule, it must be enforced by a higher-level policy.

## Correct a displayed balance

There is no balance setter. A correction records the exact delta as a new journal. First read the account and copy its current `balance_version`:

```bash
ACCOUNT="$(curl -sS "$API/accounts/$ACCOUNT_ID" \
  -H "Authorization: Bearer $TOKEN")"
BALANCE_VERSION="$(jq -r '.balance_version' <<<"$ACCOUNT")"
```

Then submit the observed target:

```bash
curl -sS -X POST "$API/accounts/$ACCOUNT_ID/balance-corrections" \
  -H "Authorization: Bearer $TOKEN" \
  -H 'Content-Type: application/json' \
  -H 'Idempotency-Key: correct-wallet-count-20260824' \
  -d "{
    \"target_display_balance\": {\"amount\": \"850.00\", \"currency\": \"UAH\"},
    \"expected_balance_version\": $BALANCE_VERSION,
    \"reason\": \"Counted physical cash\",
    \"observed_at\": \"2026-08-24T18:00:00Z\"
  }" | jq
```

The target currency must match the account. The reason must contain 1 to 500 characters. A stale version returns `409`; a target equal to the current display balance returns `400` because a zero-delta correction is not a financial event.

The resulting journal has `source: "correction"`, `purpose: "correction"`, and a populated `correction` object containing the before balance, target, delta, observed version, reason, and observation time.

## Reverse or replace history

### Reverse

A reversal creates one exact inverse journal and links it to the original:

```bash
curl -sS -X POST "$API/transactions/$JOURNAL_ID/reversals" \
  -H "Authorization: Bearer $TOKEN" \
  -H 'Content-Type: application/json' \
  -H 'Idempotency-Key: reverse-duplicate-lunch-20260824' \
  -d '{"reason":"Duplicate entry"}' | jq
```

The original is unchanged. Reading it later exposes the new ID in `reversed_by_journal_id`. Only one direct reversal may point to an original journal; another attempt returns `409`.

The implementation can reverse any tenant-visible journal, not only a manual income or expense. Reversal remains allowed when an affected account is archived.

### Replace

Replacement atomically creates two journals: an exact reversal of the original and a new manual income/expense journal with a new annotation.

```bash
curl -sS -X POST "$API/transactions/$JOURNAL_ID/replacements" \
  -H "Authorization: Bearer $TOKEN" \
  -H 'Content-Type: application/json' \
  -H 'Idempotency-Key: replace-lunch-20260824' \
  -d "{
    \"account_id\": \"$ACCOUNT_ID\",
    \"kind\": \"expense\",
    \"amount\": {\"amount\": \"120.00\", \"currency\": \"UAH\"},
    \"description\": \"Corrected lunch\",
    \"category_id\": \"$CATEGORY_ID\",
    \"note\": \"Corrected receipt\",
    \"tags\": [\"food\"],
    \"budget_visibility\": \"included\"
  }" | jq
```

The result contains `reversal_journal_entry_id`, `replacement_journal_entry_id`, combined account effects, and `replayed`. The original later exposes both `reversed_by_journal_id` and `replaced_by_journal_id`.

Because replacement consumes the original's one allowed direct reversal, an already reversed or replaced journal cannot be replaced again. The code does not restrict the original's journal type; regardless of the original shape, the replacement is always the submitted manual income/expense shape. The replacement account must be active.

## Reconciliation

The public Ledger API does not accept raw provider observations. Banking submits normalized observations internally after a provider resource has been mapped to a Ledger account.

For each observation, the Ledger captures:

- the provider-reported and optional available balances;
- the current Ledger display balance and `balance_version`;
- `delta = provider_reported - captured_ledger_balance`;
- the provider source and ordering information.

A zero delta creates a `matched` case. A non-zero delta creates a `pending` case. A newer observation on the same source stream supersedes an earlier pending case. Out-of-order observations are retained as `ignored_older` rather than becoming active.

List and inspect cases:

```bash
curl -sS "$API/reconciliations" \
  -H "Authorization: Bearer $TOKEN" | jq

curl -sS "$API/reconciliations/$CASE_ID" \
  -H "Authorization: Bearer $TOKEN" | jq
```

Cases are ordered newest first by observation time, source sequence, and observation ID.

### Approve a pending case

Copy both `case.version` and `case.captured_balance_version`:

```bash
curl -sS -X POST "$API/reconciliations/$CASE_ID/approve" \
  -H "Authorization: Bearer $TOKEN" \
  -H 'Content-Type: application/json' \
  -H 'Idempotency-Key: approve-reconciliation-20260824' \
  -d '{
    "expected_version": 1,
    "expected_balance_version": 8,
    "reason": "Matches provider statement"
  }' | jq
```

Approval succeeds only when the case is still the active pending case and the account balance version has not changed since observation. It creates an adjustment journal that moves the display balance to the observed provider balance, marks the case `approved`, and returns the journal and account effect. Any intervening financial posting makes the approval stale and returns `409`; re-read or wait for a new provider observation rather than resubmitting with a guessed version.

### Dismiss a pending case

```bash
curl -sS -X POST "$API/reconciliations/$CASE_ID/dismiss" \
  -H "Authorization: Bearer $TOKEN" \
  -H 'Content-Type: application/json' \
  -H 'Idempotency-Key: dismiss-reconciliation-20260824' \
  -d '{
    "expected_version": 1,
    "reason": "Provider statement is known to be delayed"
  }' | jq
```

Dismissal marks a pending case `dismissed` and records the reason. It creates no journal and has no balance effect.

The serialized status vocabulary is `matched`, `pending`, `superseded`, `ignored_older`, `approved`, `dismissed`, and `stale`. The current observation/decision flow produces all except `stale`; stale approval attempts are rejected with `409` while the case remains unchanged.

## Split an existing transaction and apply a repayment

Sharing uses Ledger corrections; it never edits an imported transaction. There is no separate "create bill from transaction" endpoint and no automatic transaction matching. The client selects journal IDs and submits them through the existing bill and settlement endpoints.

The standard 1,000/500 UAH flow is:

1. A 1,000 UAH outgoing provider transaction has already been imported into Ledger.
2. Create a bill whose current-user contribution is 1,000 UAH and whose evidence allocates that complete outgoing journal.
3. Give the current user a 500 UAH share and a contact a 500 UAH share.
4. Poll the bill until accounting becomes `active`, or `failed` with `accounting_error`.
5. Later select an incoming transaction and create a settlement against the exact contact-to-user obligation.
6. Poll the settlement list until the settlement becomes `posted` or `failed`.

Create the bill:

```bash
curl -sS -X POST "$API/bill-splits" \
  -H "Authorization: Bearer $TOKEN" \
  -H 'Content-Type: application/json' \
  -H 'Idempotency-Key: split-dinner-20260824' \
  -d "{
    \"title\": \"Dinner\",
    \"occurred_at\": \"2026-08-24T18:30:00Z\",
    \"total\": {\"amount\": \"1000.00\", \"currency\": \"UAH\"},
    \"contributions\": [{
      \"participant\": {\"kind\": \"current_user\"},
      \"amount\": {\"amount\": \"1000.00\", \"currency\": \"UAH\"},
      \"evidence\": {
        \"kind\": \"existing_journals\",
        \"allocations\": [{
          \"journal_id\": \"$OUTGOING_JOURNAL_ID\",
          \"amount\": {\"amount\": \"1000.00\", \"currency\": \"UAH\"}
        }]
      }
    }],
    \"shares\": {
      \"kind\": \"exact\",
      \"shares\": [
        {
          \"participant\": {\"kind\": \"current_user\"},
          \"amount\": {\"amount\": \"500.00\", \"currency\": \"UAH\"}
        },
        {
          \"participant\": {\"kind\": \"contact\", \"contact_id\": \"$CONTACT_ID\"},
          \"amount\": {\"amount\": \"500.00\", \"currency\": \"UAH\"}
        }
      ]
    }
  }" | jq
```

The command returns `202`. Its initial bill status is `pending_accounting`. Poll:

```bash
curl -sS "$API/bill-splits/$BILL_ID" \
  -H "Authorization: Bearer $TOKEN" | jq
```

The read model returns `current_revision`, `accounted_revision`, `accounting_error`, and `fully_settled`. `allocations.contributions[].evidence.allocations` preserves every selected journal and amount. Each obligation includes `amount`, `settled_amount`, and `remaining_amount`.

For this bill, the worker appends a correction linked through `correction_of` to the selected outgoing journal:

```text
External receivable  +500.00 UAH
Expense              -500.00 UAH
```

The original outgoing journal remains an ordinary 1,000 UAH expense. Ledger records the source journal, correction journal, amount, currency, and source nature in its reclassification details. The selected source is locked while capacity is checked. Active corrections cannot allocate more than the transaction's eligible expense amount; reversing a correction restores that capacity.

A selected outgoing journal is rejected without a correction if it belongs to another tenant, has another currency, is not an expense, is reversed or replaced, is not an imported/manual ordinary transaction, or lacks sufficient unallocated amount. Manual contribution accounts are likewise checked for tenant, currency, and active lifecycle before bill effects are posted.

When the contact later sends 500 UAH and that incoming transaction appears in Ledger, apply it manually:

```bash
curl -sS -X POST "$API/bill-splits/$BILL_ID/settlements" \
  -H "Authorization: Bearer $TOKEN" \
  -H 'Content-Type: application/json' \
  -H 'Idempotency-Key: settle-dinner-20260825' \
  -d "{
    \"expected_version\": $BILL_VERSION,
    \"debtor\": {\"kind\": \"contact\", \"contact_id\": \"$CONTACT_ID\"},
    \"creditor\": {\"kind\": \"current_user\"},
    \"amount\": {\"amount\": \"500.00\", \"currency\": \"UAH\"},
    \"evidence\": {
      \"kind\": \"existing_journal\",
      \"journal_id\": \"$INCOMING_JOURNAL_ID\"
    },
    \"occurred_at\": \"2026-08-25T10:15:00Z\"
  }" | jq
```

The exact debtor/creditor pair must identify a current bill obligation. The amount may be partial but cannot exceed `remaining_amount`. For example, the same 500 UAH incoming journal can back a 200 UAH settlement followed by a 300 UAH settlement, subject to both obligation and journal capacity.

Poll all settlements for the bill:

```bash
curl -sS "$API/bill-splits/$BILL_ID/settlements" \
  -H "Authorization: Bearer $TOKEN" | jq
```

Each item exposes debtor, creditor, amount, evidence including the selected journal, occurrence time, status, process error, and `accounting_journal_id`. A successful selected incoming repayment appends this linked correction:

```text
Income               -500.00 UAH
External receivable  -500.00 UAH
```

These are display effects. The income line's raw signed posting is positive because income is credit-normal. The original incoming journal remains unchanged. After the correction, both the income created by that import and the 500 UAH receivable are zero.

If selected settlement evidence permanently fails validation, the settlement becomes `failed`; its reserved obligation amount and the bill's active-settlement count are released so another transaction can be selected. Contact-to-contact and explicit `external` settlements finish without Ledger effects.

Bill revisions append a new immutable revision. The worker stores all journals generated for each revision, reverses all accounting journals from the latest posted revision, and then activates the replacement. Cancellation reverses every journal from the latest posted revision. Settlement reversal reverses its exact accounting journal and restores both the obligation and selected-journal capacity.

The Sharing workflow runs inside the `process-manager-retries` runtime. It claims one due process at a time with a fenced 30-second lease. An abandoned `processing` process becomes claimable after lease expiry. Transient persistence failures retry with exponential delays from one second up to five minutes. Ledger commands use stable, payload-sensitive idempotency keys, so a crash after Ledger commits but before Sharing finalizes replays the same journal rather than duplicating it.

## Endpoint reference

| Method | Path | Success | Purpose |
| --- | --- | ---: | --- |
| `POST` | `/accounts` | `201` | Open a manual account and optionally post its opening balance |
| `GET` | `/accounts` | `200` | List user-visible active and archived accounts |
| `GET` | `/accounts/{id}` | `200` | Get one account and current balance |
| `PATCH` | `/accounts/{id}` | `200` | Rename using `account.version` |
| `POST` | `/accounts/{id}/archive` | `200` | Archive using `account.version` |
| `POST` | `/accounts/{id}/restore` | `200` | Restore using `account.version` |
| `GET` | `/accounts/{id}/activity` | `200` | List journals touching one account |
| `POST` | `/transactions` | `201` | Record manual income or expense |
| `GET` | `/transactions` | `200` | List all tenant journals |
| `GET` | `/transactions/{id}` | `200` | Get journal, postings, annotation, relations, and correction detail |
| `PATCH` | `/transactions/{id}/annotation` | `200` | Edit mutable metadata using `annotation.version` |
| `POST` | `/transactions/{id}/reversals` | `201` | Create an exact inverse journal |
| `POST` | `/transactions/{id}/replacements` | `201` | Reverse and create a manual replacement atomically |
| `POST` | `/transfers` | `201` | Transfer same-currency or cross-currency funds |
| `POST` | `/accounts/{id}/balance-corrections` | `201` | Post the delta to a target display balance |
| `GET` | `/reconciliations` | `200` | List provider reconciliation cases |
| `GET` | `/reconciliations/{id}` | `200` | Get one reconciliation case |
| `POST` | `/reconciliations/{id}/approve` | `200` | Approve a pending, version-fenced case |
| `POST` | `/reconciliations/{id}/dismiss` | `200` | Dismiss a pending case without accounting effects |
| `POST` | `/bill-splits` | `202` | Create a bill and queue durable Sharing accounting |
| `GET` | `/bill-splits/{id}` | `200` | Poll bill revision, accounting error, allocations, and remaining obligations |
| `POST` | `/bill-splits/{id}/settlements` | `202` | Reserve and queue a full or partial obligation settlement |
| `GET` | `/bill-splits/{id}/settlements` | `200` | Poll selected evidence, errors, and settlement accounting journals |

There are deliberately no hard-delete account/transaction routes and no `PATCH /accounts/{id}/balance` route. Those paths return `404`.

## Operational integrity

Every journal commit stores the journal, optional annotation, balance projection updates, audit records, outbox events, and idempotency receipt in one database transaction.

The Ledger facade also provides non-HTTP operational capabilities:

- `verify_projection()` compares every projected signed balance with the sum of immutable postings and returns exact mismatches.
- `rebuild_projection()` takes an exclusive lock on the balance projection and recomputes it from postings, incrementing projection versions.

These are application-level maintenance methods, not public HTTP endpoints. Do not repair financial history by updating journal entries or postings directly. If the projection is wrong, rebuild the projection from immutable facts; if a financial fact is wrong, record a correction, reversal, or replacement.
