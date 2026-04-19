# Monobank Sync — Design

**Date:** 2026-03-11

## Overview

Sync transactions from Monobank into moneykeeper accounts. Each user provides their own Monobank API token, picks a specific card to link to a moneykeeper account, gets a full history pull on connect, and receives new transactions via Monobank webhook going forward.

---

## Data Model

### New table: `monobank_connections`

| column | type | notes |
|---|---|---|
| `id` | TEXT (UUID) | PK |
| `account_id` | TEXT | FK → accounts |
| `user_id` | TEXT | FK → users |
| `token` | TEXT | Monobank API token |
| `monobank_account_id` | TEXT | specific card ID from Monobank |
| `sync_status` | TEXT | `pending` / `syncing` / `completed` / `failed` |
| `last_synced_at` | INTEGER | Unix timestamp, nullable |
| `created_at` | INTEGER | Unix timestamp |

### Modified table: `transactions`

Add column: `external_id TEXT UNIQUE NULL` — stores Monobank transaction ID for deduplication via `INSERT OR IGNORE`.

---

## API Endpoints

All endpoints require JWT auth except the webhook.

| method | path | purpose |
|---|---|---|
| `GET` | `/monobank/client-info` | fetch available Monobank cards (token passed as `X-Token` header, proxied to Monobank) |
| `POST` | `/monobank/connect` | link a moneykeeper account to a Monobank card; triggers background sync + sets webhook |
| `GET` | `/monobank/connections` | list current user's connections with sync status |
| `DELETE` | `/monobank/connections/:id` | remove a connection |
| `POST` | `/monobank/webhook` | public — receives new transactions pushed by Monobank |

### `POST /monobank/connect` body

```json
{
  "account_id": "<moneykeeper account UUID>",
  "token": "<monobank api token>",
  "monobank_account_id": "<monobank card id>"
}
```

### `POST /monobank/webhook` payload (from Monobank)

```json
{
  "type": "StatementItem",
  "data": {
    "account": "<monobank_account_id>",
    "statementItem": { ... }
  }
}
```

The connection is looked up by `monobank_account_id` from the payload.

---

## Background Sync

### On `POST /monobank/connect`

1. Save connection with `sync_status = pending`
2. Return 201 immediately
3. Spawn `tokio::task`:
   - Set `sync_status = syncing`
   - Call `POST https://api.monobank.ua/personal/webhook` to register webhook URL
   - Fetch transaction history in 31-day chunks, oldest-first, from account `created_at` to now
   - Sleep 61 seconds between chunk requests (Monobank rate limit: 1 req/min)
   - Insert each transaction with `INSERT OR IGNORE` (dedup via `external_id`)
   - On success: set `sync_status = completed`, update `last_synced_at`
   - On failure: set `sync_status = failed`

### On application startup

1. Query all connections with `sync_status IN ('pending', 'syncing')`
2. Reset to `pending`
3. Re-spawn background sync task for each

This handles crash recovery safely — `INSERT OR IGNORE` makes re-fetching already-synced chunks idempotent.

### Webhook URL

Configured via `PUBLIC_URL` env var (e.g. `https://myserver.com`). Full URL: `{PUBLIC_URL}/monobank/webhook`.

---

## Transaction Mapping

| Monobank field | Our field | notes |
|---|---|---|
| `id` | `external_id` | deduplication |
| `time` | `date` | Unix timestamp |
| `description` | `description` | merchant name / note |
| `amount` | `amount` | divide by 100 (kopecks → UAH) |
| sign of `amount` | `transaction_type` | negative = `expense`, positive = `income` |
| — | `account_id` | from connection record |
| — | `category_id` | `NULL` — user categorizes manually |

---

## New Dependencies

- `reqwest` — HTTP client for Monobank API calls (with `rustls-tls` feature)

---

## Environment Variables

| var | purpose |
|---|---|
| `PUBLIC_URL` | Base URL registered as Monobank webhook (e.g. `https://myserver.com`) |
