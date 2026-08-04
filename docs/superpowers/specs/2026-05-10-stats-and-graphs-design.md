# Stats & Graphs API — Design

**Date:** 2026-05-10
**Status:** Approved (brainstormed)

## Goal

Expose aggregated, currency-normalized data so the frontend can render the
following views with a single round-trip per view:

- Dashboard summary widgets (net worth, this-month income/expense, top categories)
- Net-worth line chart over time
- Income vs expense bar chart (cashflow)
- Category breakdown pie/donut
- Investments view (per-ticker holdings, cost basis, realized P&L, staking)

Out of scope for this iteration: account comparison view, live ticker prices /
unrealized P&L, FIFO cost accounting, account-level filtering on stats
endpoints (can be added later).

## Decisions (locked in)

| Decision | Choice |
|---|---|
| Client model | Frontend we control — endpoints shaped per view |
| Currency handling | Convert to a single base currency (no per-currency arrays) |
| FX source | Daily snapshot table, populated from NBU |
| Base currency | User setting + per-request `?base_currency=` override |
| Investments depth | Cost-basis only, no live prices |
| Time granularity | `day` / `month` / `year` (no week) |
| Aggregation strategy | Query-time SQL + short HTTP cache (`max-age=60`) |

## Endpoints

All under `/stats`, all authenticated, all scoped to caller's `user_id`.
Common query params: `from`, `to` (Unix seconds), `base_currency` (override).
Responses always echo the resolved `base_currency`.

| Endpoint | Purpose | View-specific params |
|---|---|---|
| `GET /stats/dashboard` | Single-shot snapshot: net worth (now), this-month income & expense totals, top N expense categories this month | `top_n` (default 5) |
| `GET /stats/balance-history` | Net-worth line: one running-total point per period | `granularity` |
| `GET /stats/cashflow` | Income vs expense bars per period | `granularity` |
| `GET /stats/categories` | Category breakdown for the date range | `kind` = `expense` \| `income` (default `expense`) |
| `GET /stats/investments` | Per-ticker holdings, cost basis, realized P&L, staking rewards | none |

Defaults: missing `from` → user's earliest transaction; missing `to` → now.

All responses set `Cache-Control: private, max-age=60`.

### Response shapes (JSON)

```jsonc
// GET /stats/dashboard
{
  "base_currency": "UAH",
  "net_worth": "123456.78",
  "month": { "from": 1714521600, "to": 1717199999,
             "income": "20000", "expense": "12500" },
  "top_categories": [
    { "category_id": "uuid|null", "name": "Groceries", "total": "5400" }
  ],
  "partial": false
}

// GET /stats/balance-history?granularity=month
{
  "base_currency": "UAH",
  "granularity": "month",
  "points": [
    { "period_start": 1704067200, "balance": "98000" },
    { "period_start": 1706745600, "balance": "112500" }
  ],
  "partial": false
}

// GET /stats/cashflow?granularity=month
{
  "base_currency": "UAH",
  "granularity": "month",
  "points": [
    { "period_start": 1704067200, "income": "20000", "expense": "15000" }
  ],
  "partial": false
}

// GET /stats/categories?kind=expense
{
  "base_currency": "UAH",
  "kind": "expense",
  "items": [
    { "category_id": "uuid|null", "name": "Groceries", "total": "5400" }
  ],
  "partial": false
}

// GET /stats/investments
{
  "base_currency": "UAH",
  "tickers": [
    {
      "ticker": "BTC",
      "holdings": "0.42",                   // ticker units
      "cost_basis": "650000",               // base currency
      "realized_pnl": "12000",              // base currency, average cost
      "staking_received": "0.005"           // ticker units
    }
  ],
  "partial": false
}
```

`partial: true` indicates at least one transaction could not be converted
because no FX rate was available; the response also carries
`missing_rates: [{ "date": "YYYY-MM-DD", "currency": "XYZ" }]` (truncated to
the first ~10).

## FX rates

### Schema

```sql
CREATE TABLE fx_rates (
    rate_date     DATE    NOT NULL,
    from_currency TEXT    NOT NULL,
    to_currency   TEXT    NOT NULL,
    rate          NUMERIC NOT NULL,
    PRIMARY KEY (rate_date, from_currency, to_currency)
);
CREATE INDEX idx_fx_rates_date ON fx_rates (rate_date);
```

Rates are stored canonically as `from_currency → UAH`. Cross rates are
derived at query time: `USD → EUR = (USD→UAH) / (EUR→UAH)`.

### Source

NBU (`bank.gov.ua`) public API: free, no auth, returns daily rates against
UAH. A `FxRateSource` trait in `application/` lets us swap implementations
for tests.

### Sync

`application/fx_sync.rs::FxSyncUseCase`:

- `sync_today()` — fetch today's rates from NBU, upsert.
- `backfill(from, to)` — fetch a date range; used once on first deploy and
  to fill gaps detected on startup.

Scheduling: a tokio task spawned in `main.rs` after the HTTP server starts.
Runs `sync_today()` on startup and then every 24h (~01:00 UTC). On error,
log and retry on the next tick — no external scheduler required.

### Lookup rule

For a transaction on date `D` in currency `C` converted to base `B`:

1. Look up `(D, C, UAH)` and `(D, B, UAH)`. Compute `rate = C→UAH / B→UAH`.
2. If the exact date is missing, fall back to the most recent earlier date
   (`SELECT … WHERE rate_date <= D ORDER BY rate_date DESC LIMIT 1`).
3. If `C == B` or one side is `UAH`, simplify accordingly (identity or
   single lookup).
4. If still no rate, surface the transaction's amount unconverted and set
   `partial: true` on the response.

In SQL this is implemented as a CTE that materializes the rate-as-of-date
per `(date, currency)` once, then the main aggregation joins against it —
avoiding a correlated subquery per row.

## User base currency

### Schema

There is no `users` table in this app — `user_id` comes from the Supabase
JWT. So we add a new `user_settings` table keyed by `user_id`:

```sql
CREATE TABLE user_settings (
    user_id       UUID PRIMARY KEY NOT NULL,
    base_currency TEXT NOT NULL DEFAULT 'UAH',
    updated_at    TIMESTAMPTZ NOT NULL
);
```

Rows are created lazily on first `PATCH /me/settings`. When no row exists
for a user, the default `UAH` is used.

`base_currency` is a 3-letter ISO code, validated on update against the
`fx_rates` table (or `UAH`).

### API

- `GET /me/settings` — returns `{ "base_currency": "..." }`.
- `PATCH /me/settings` — accepts `{ "base_currency": "USD" }`.

### Resolution per stats request

1. If `?base_currency=XYZ` is present and valid → use it.
2. Else use `users.base_currency`.
3. If invalid (unknown code, or no FX rates for it) → `400 InvalidInput`.

## Architecture

Following the existing DDD layout (`domain` → `application` → `infrastructure`
+ `api`).

### `domain/`

- `domain/stats.rs` — result types (`DashboardStats`, `BalanceHistoryPoint`,
  `CashflowPoint`, `CategoryBreakdownItem`, `TickerHolding`, etc.) and a
  `StatsRepository` trait with one method per endpoint.
- `domain/fx_rate.rs` — `FxRate` entity and `FxRateRepository` trait
  (`get_rate(date, from, to)`, `upsert_many(rates)`, `latest_date()`,
  `currencies()` for validation).

### `application/`

- `application/stats.rs` — `StatsService` takes `StatsRepository`,
  `FxRateRepository`, and `UserRepository`. Resolves the base currency,
  delegates to the repository, sets `partial`/`missing_rates` from the
  repository's report.
- `application/fx_sync.rs` — `FxSyncUseCase` with `sync_today()` and
  `backfill(from, to)`. Depends on an `FxRateSource` trait.

### `infrastructure/`

- `infrastructure/stats_repository.rs` — pure SQL, one query per endpoint,
  joining `transactions` to `fx_rates` via the rate-as-of-date CTE.
- `infrastructure/fx_rate_repository.rs` — CRUD plus the rate-as-of-date
  lookup helper.
- `infrastructure/nbu_client.rs` — HTTP client implementing `FxRateSource`.

### `api/`

- `api/handlers/stats.rs` — five thin handlers: parse params, call service,
  return JSON with `Cache-Control: private, max-age=60`.
- `api/dto.rs` — response types per endpoint.
- `api/routes.rs` — register `/stats/*` and the `/users/me` PATCH route.

### `main.rs`

Spawn the FX sync background task after the server is wired up.

## Data flow & queries

```
HTTP request
  → handler (parse params, auth)
  → StatsService (resolve base currency, dispatch)
  → StatsRepository (single SQL query: txs ⨝ fx_rates)
  → handler returns JSON + Cache-Control
```

### Balance history

For each period bucket from `from` to `to`:

1. Sum signed amounts: Income/Sell/StakingReward positive, Expense/Buy
   negative. Transfer is stored as a pair of linked transactions (one per
   account); summed across all of a user's accounts the pair must net to
   zero. **Verify during implementation** how the sign is actually stored
   for each Transfer leg before writing the aggregation SQL.
2. Convert each transaction to base currency at its `transacted_at` date
   FX rate.
3. Add accounts' `initial_balance`, converted at the account's creation
   date.

Implementation: `SELECT date_trunc(<gran>, transacted_at) AS bucket,
SUM(signed_amount * rate) AS delta FROM … GROUP BY bucket`, then a
`SUM() OVER (ORDER BY bucket)` to produce a running total seeded with the
sum of converted initial balances.

### Cashflow

```sql
SELECT date_trunc(<gran>, transacted_at) AS bucket,
       kind,
       SUM(amount * rate) AS total
FROM transactions JOIN rate_cte USING (transacted_at::date, currency)
WHERE user_id = $1 AND kind IN ('Income', 'Expense') AND …
GROUP BY bucket, kind
```

Service pivots into `{period_start, income, expense}` rows.

### Categories

```sql
SELECT category_id,
       SUM(amount * rate) AS total
FROM transactions JOIN rate_cte …
WHERE user_id = $1 AND kind = $2 AND …
GROUP BY category_id
ORDER BY total DESC
```

Join `categories` for names. NULL `category_id` → label `Uncategorized`.

### Investments

Two passes combined in the service:

- **Aggregates per ticker** (single SQL):
  ```sql
  SELECT ticker,
         SUM(CASE kind WHEN 'Buy'  THEN quantity
                       WHEN 'Sell' THEN -quantity
                       ELSE 0 END) AS holdings,
         SUM(CASE kind WHEN 'Buy'  THEN amount * rate
                       ELSE 0 END) AS gross_invested,
         SUM(CASE kind WHEN 'StakingReward' THEN quantity
                       ELSE 0 END) AS staking_qty
  FROM transactions JOIN trade_details ON …
                    JOIN rate_cte ON …
  WHERE user_id = $1
  GROUP BY ticker
  ```

- **Realized P&L (average cost) in service code**: walk each ticker's
  trade history in `transacted_at` order, maintain `(qty_held, total_cost)`,
  on each Sell compute
  `realized += sell_amount_in_base − (total_cost / qty_held) * sell_qty`
  and decrement `qty_held` and `total_cost` proportionally. This is
  simpler to test than a windowed SQL version and the data volumes are
  small (per-user ticker history).

`cost_basis` reported to the client is the average-cost basis of the
*remaining* holdings, i.e. `total_cost` after walking all trades.

## Errors & edge cases

- Invalid `base_currency` (unknown / no FX rates) → `400 InvalidInput`.
- Invalid date range (`from > to`, out-of-range Unix timestamp) → `400`
  via the existing `parse_date_range` helper.
- Invalid `granularity` → `400`.
- NBU fetch failure during sync → log, retry next cycle. Stats endpoints
  stay up; rates just go stale.
- DB errors → existing `AppError` flow.

Edge cases:

- **No transactions** — empty arrays / zero totals, never `404`.
- **Mixed currencies, missing rate for some dates** — per Section "Lookup
  rule"; response carries `partial: true` and `missing_rates`.
- **Open-ended date range** — `from` defaults to user's earliest tx; `to`
  defaults to now.
- **Tickers in different currencies** — cost basis is the *account*
  currency converted to base on the trade date; holdings stay in ticker
  units.
- **Deleted/absent categories** — grouped as `Uncategorized`.
- **Timezone** — bucket boundaries are UTC for v1. Adding `?tz=` is a
  known follow-up.

## Testing

Following existing patterns (real Postgres in `tests/api/` via
`infrastructure/test_db.rs`).

### Unit

- `application/stats.rs` — base currency resolution, `partial` flag
  propagation, average-cost realized P&L (numeric examples worked by
  hand).
- `application/fx_sync.rs` — uses a fake `FxRateSource`, asserts upsert
  and idempotency.
- `infrastructure/fx_rate_repository.rs` — rate-as-of-date fallback
  behavior.

### Integration (`tests/api/stats.rs`)

For each endpoint:

- Happy path, single currency.
- Multi-currency with seeded `fx_rates`.
- Empty data.
- Date-range boundaries (txs at `from` and `to` included).
- `?base_currency=` override beats user setting.

Plus, for investments: a scenario with Buy → Buy → Sell → StakingReward
with hand-checked numbers for `holdings`, `cost_basis`, `realized_pnl`,
and `staking_received`.

### FX sync

A test wiring `FxSyncUseCase` to a fake source returning a canned
response, asserting upsert + idempotency on re-run.

## Migration & rollout

1. Migration: create `fx_rates` table, add `base_currency` column.
2. Backfill `fx_rates` for the past ~5 years (or oldest user transaction,
   whichever is later) on first deploy.
3. Ship endpoints; client cuts over view by view.
4. Monitor `partial: true` rate; if non-zero in steady state, expand
   backfill range.

## Open questions / known follow-ups

- **Timezone-aware buckets** — `?tz=Europe/Kyiv` for users outside UTC.
- **Live ticker prices / unrealized P&L** — separate price-feed subsystem.
- **Account filter on stats endpoints** — if users want per-account charts.
- **ETag-based caching** — if `max-age=60` proves too coarse.
