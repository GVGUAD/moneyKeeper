# Supabase Migration Design

**Date:** 2026-03-12
**Status:** Approved

## Overview

Full migration from SQLite + self-hosted auth to Supabase: Supabase Auth replaces the local auth layer, and Supabase-hosted PostgreSQL replaces SQLite. The existing DDD architecture, repository pattern, and domain model remain intact. The migration is a database driver swap + SQL dialect adaptation + auth layer removal.

## Goals

- Replace SQLite with Supabase-hosted PostgreSQL
- Replace self-hosted email/password auth with Supabase Auth
- Support mobile and web clients via Supabase Auth JWTs
- Deploy backend to Fly.io

## Scope

This is a personal project with no existing users to preserve. **Data migration is out of scope** — the PostgreSQL schema starts fresh. All migrations are rewritten from scratch; the old SQLite migration history is discarded.

Only email/password and OAuth providers supported by Supabase Auth are in scope. Phone-number sign-in is out of scope.

## Approach

Option A: Drop-in swap — `sqlx postgres` + Supabase Auth JWT verification. Keep the existing repository architecture; swap the sqlx driver, adapt SQL syntax, remove the auth layer, verify Supabase-issued JWTs in middleware.

## What Gets Deleted

| Item | Reason |
|---|---|
| `src/domain/user.rs` | `User`, `RefreshToken`, `UserRepository` obsolete |
| `src/application/auth.rs` | `AuthService` no longer needed |
| `src/api/handlers/auth.rs` | Auth endpoints removed entirely |
| `src/infrastructure/user_repository.rs` | No local user storage |
| Cargo deps: `argon2`, `rand`, `sha2`, `hex` | Auth deps removed |
| All existing migration files | Replaced by fresh PostgreSQL migrations |

## Dependency Changes

```toml
# Cargo.toml — update sqlx features
sqlx = { version = "0.8", features = ["runtime-tokio", "postgres", "migrate", "uuid", "chrono", "rust_decimal"] }

# Already present — no changes needed
jsonwebtoken = "9"
rust_decimal = { version = "1", features = ["serde-with-str"] }

# Remove
argon2 = "..."
rand = "..."
sha2 = "..."
hex = "..."
```

Environment variables (rename `JWT_SECRET` → `SUPABASE_JWT_SECRET` in `.env` and all configs):
```
# Port 5432 = direct Postgres connection (session mode) — correct for a persistent server.
# Do NOT use port 6543 (PgBouncer) — that is for serverless/short-lived connections.
DATABASE_URL=postgresql://postgres:[password]@db.[project-ref].supabase.co:5432/postgres
SUPABASE_JWT_SECRET=<Supabase Dashboard → Settings → API → JWT Secret>
PUBLIC_URL=https://[your-app].fly.dev
BIND_ADDR=0.0.0.0:8080
```

The backend connects via the Postgres wire protocol (port 5432), not the Supabase REST API. No Supabase `anon` or `service_role` API key is needed.

## Row-Level Security

RLS is **disabled** on all tables. The backend enforces data isolation by including `WHERE user_id = $1` in every query — the same approach as the SQLite version. Supabase's default is RLS off; no policy configuration is required.

## SQL Migrations — Fresh PostgreSQL Schema

All migrations are rewritten for PostgreSQL from scratch using sqlx's naming convention: `NNNN_description.sql` (e.g., `0001_accounts.sql`, `0002_transactions.sql`). The `_sqlx_migrations` table starts clean on the new Supabase database.

Key type changes from the old SQLite schema:

| SQLite | PostgreSQL |
|---|---|
| `TEXT` (UUID) | `UUID` |
| `TEXT` (timestamp) | `TIMESTAMPTZ` |
| `REAL` | `NUMERIC` |
| `INTEGER` | `BIGINT` |
| `INSERT OR IGNORE` | `INSERT ... ON CONFLICT DO NOTHING` |

**No `users` or `refresh_tokens` tables** — Supabase Auth owns user management. The `user_id` columns store the Supabase user UUID as a bare `UUID` with no FK constraint (no local `users` table to reference).

**Monobank idempotent insert:** The transactions migration must include a partial unique index:
```sql
CREATE UNIQUE INDEX transactions_external_id_unique
    ON transactions (external_id)
    WHERE external_id IS NOT NULL;
```

This index enables the conflict target in the idempotent insert:
```sql
INSERT INTO transactions (...) VALUES (...)
ON CONFLICT (external_id) WHERE external_id IS NOT NULL DO NOTHING
```

`sqlx::migrate!()` in `db.rs` is unchanged — migrations run automatically on startup.

## Repository Changes

All 4 remaining repositories (`account`, `transaction`, `category`, `monobank`) require:

1. **Pool type:** `SqlitePool` → `PgPool`
2. **UUID columns:** Bind `Uuid` directly (sqlx `uuid` feature) — remove manual `to_string()` / `parse_str()`
3. **Timestamps (account, transaction, category):** These repos store timestamps as RFC3339 `TEXT`. Bind `DateTime<Utc>` directly via the sqlx `chrono` feature and use `TIMESTAMPTZ` columns — remove RFC3339 string conversion.
4. **Timestamps (monobank):** `monobank_repository.rs` stores timestamps as Unix epoch `i64` (not RFC3339). Keep `BIGINT` columns and the existing `.timestamp()` / `DateTime::from_timestamp()` pattern — no change needed.
5. **Decimals:** sqlx `rust_decimal` feature handles `NUMERIC` ↔ `Decimal` mapping — no code change needed, just the feature flag
6. **Idempotent insert:** `INSERT OR IGNORE` → `INSERT ... ON CONFLICT (external_id) WHERE external_id IS NOT NULL DO NOTHING`
7. **Dynamic SQL placeholder syntax:** SQLite uses `?` for all bind parameters; PostgreSQL requires numbered placeholders `$1`, `$2`, etc. The `list` method in `transaction_repository.rs` and `compute_balance` in `account_repository.rs` both build SQL strings dynamically at runtime using `?`. These must be rewritten to emit `$1`, `$2`, ... using a counter that increments as optional conditions are appended. sqlx compile-time macros do not catch this — it panics at runtime.

### Testing

Every existing repository unit test uses `SqlitePool::connect("sqlite::memory:")` and `SqliteUserRepository`. Once SQLite is removed, those tests fail to compile. The replacement strategy:

- Use the `#[sqlx::test]` macro — it spins up a temporary PostgreSQL database per test, applies migrations automatically, and tears it down after. Requires `DATABASE_URL` to point at a Postgres instance during `cargo test`.
- For CI/local dev: use a local Docker PostgreSQL or the Supabase project URL. `sqlx::test` will create/drop databases automatically.
- The `user_id` setup in existing tests (inserting a `User` to satisfy FK constraints) is replaced by inserting a random `Uuid::new_v4()` directly — no FK target needed since the `users` table is gone.

`#[sqlx::test]` requires sqlx in `[dev-dependencies]` as well as `[dependencies]`:
```toml
[dev-dependencies]
sqlx = { version = "0.8", features = ["runtime-tokio", "postgres", "migrate", "uuid", "chrono", "rust_decimal"] }
```

### sqlx Offline Mode

Compile-time query checking requires a live DB at build time. Use offline mode so Fly.io builds work without a live connection:

1. Export `DATABASE_URL` pointing to Supabase and run:
   ```
   DATABASE_URL=postgresql://... cargo sqlx prepare
   ```
   (`cargo sqlx prepare` reads from the environment, not `.env` — export it explicitly or prefix the command)
2. This generates a `.sqlx/` cache directory — commit it to git
3. Set `SQLX_OFFLINE=true` in the Dockerfile:
   ```dockerfile
   ENV SQLX_OFFLINE=true
   ```
   This flag only affects compile-time macro expansion. It has no effect on runtime query execution. Without it at build time, the compiler ignores the cache and tries to connect to the DB, causing the build to fail.

Also set `max_connections` explicitly when creating `PgPool` — Supabase free tier limits to ~60 connections and Fly.io may run multiple instances:
```rust
PgPoolOptions::new()
    .max_connections(5)
    .connect(&database_url)
    .await
```

## Auth Middleware

### `src/api/jwt.rs`

Update `Claims` to match Supabase JWT structure. Supabase encodes `aud` as a JSON array (`["authenticated"]`), so the field must be `Vec<String>`. The `jsonwebtoken` v9 `Validation` must explicitly set `algorithms = [HS256]` and `aud = {"authenticated"}`:

```rust
pub struct Claims {
    pub sub: String,           // Supabase user UUID — parsed to Uuid in middleware
    pub email: Option<String>, // absent for some OAuth providers
    pub role: String,          // "authenticated"
    pub aud: Vec<String>,      // ["authenticated"]
    pub exp: i64,
    pub iat: i64,
}
```

`Validation` configuration:
```rust
let mut validation = Validation::new(Algorithm::HS256);
validation.set_audience(&["authenticated"]);
// validate_exp is true by default — leave it
// Note: jsonwebtoken v9 validates aud if set_audience() is called.
// Supabase encodes aud as a JSON array ["authenticated"] — Vec<String> handles this correctly.
```

### `src/api/middleware.rs`

- Read secret from `state.supabase_jwt_secret`
- Parse `claims.sub` → `Uuid::parse_str(&claims.sub)` to produce `AuthUser(Uuid)` — this parse must be explicit; sqlx does not do it automatically
- Downstream handlers receive `AuthUser(Uuid)` unchanged

### `src/api/state.rs`

- Remove `auth: Arc<AuthService>` and `jwt_secret: String`
- Add `supabase_jwt_secret: String`

### `src/api/routes.rs`

- Remove `/auth/*` route group

### `src/main.rs`

- Remove `AuthService` wiring
- Load `SUPABASE_JWT_SECRET` instead of `JWT_SECRET`
- Swap `SqlitePool` → `PgPool`

## Health Check Endpoint

The app currently has no `/health` endpoint. Add one as part of this migration — Fly.io requires it to confirm the app is ready before marking a deployment successful (especially important since migrations run on startup).

Add `GET /health` → `200 OK` with body `"ok"`. No auth required. Wire it in `src/api/routes.rs` before the auth middleware.

## Fly.io Deployment

Set secrets via `fly secrets set`:
```
DATABASE_URL=...
SUPABASE_JWT_SECRET=...
PUBLIC_URL=...
BIND_ADDR=0.0.0.0:8080
```

The existing `Dockerfile` must be updated (not just adding `SQLX_OFFLINE=true`):

```dockerfile
# ─── Build Stage ─────────────────────────────────────────────────────────────
FROM rust:1-alpine AS builder

RUN apk add --no-cache musl-dev pkgconf
# Removed: sqlite-dev, sqlite-static (no longer needed)

WORKDIR /app

ENV SQLX_OFFLINE=true

COPY Cargo.toml Cargo.lock ./
RUN mkdir -p src && \
    echo "fn main() {}" > src/main.rs && \
    touch src/lib.rs && \
    cargo build --release 2>/dev/null || true && \
    rm -rf src

COPY .sqlx ./.sqlx
COPY src ./src
RUN touch src/main.rs src/lib.rs && cargo build --release

# ─── Runtime Stage ───────────────────────────────────────────────────────────
FROM alpine:3.21
RUN apk add --no-cache ca-certificates tzdata
WORKDIR /app
COPY --from=builder /app/target/release/moneykeeper .

ENV RUST_LOG=info
ENV BIND_ADDR=0.0.0.0:8080
# DATABASE_URL and SUPABASE_JWT_SECRET are injected via fly secrets — not set here
# Removed: DATABASE_URL default, JWT_SECRET, VOLUME /data

EXPOSE 8080
ENTRYPOINT ["./moneykeeper"]
```

Relevant `fly.toml` sections:
```toml
[http_service]
  internal_port = 8080
  force_https = true

[[vm]]
  memory = "256mb"
  cpu_kind = "shared"
  cpus = 1
```

No separate migration step — `sqlx::migrate!()` runs on startup. The `.sqlx/` cache must be committed and `SQLX_OFFLINE=true` set in the Dockerfile for builds to succeed.

If a deployment fails mid-migration and crashes, Fly.io will automatically roll back to the previous image. Migrations are idempotent by design (sqlx tracks applied migrations in `_sqlx_migrations` and skips already-applied ones).

## Monobank Webhook — Unauthenticated Endpoint

`POST /monobank/webhook` is a public endpoint (no auth). It receives Monobank callbacks and looks up the relevant `MonobankConnection` by `monobank_account_id`, then finds the associated `user_id` from that record. Data isolation is preserved — `user_id` comes from the stored connection, not the request. This endpoint is unaffected by the auth changes.

## What Stays the Same

- All business logic: accounts, transactions, categories, Monobank sync
- `AuthUser(Uuid)` extractor shape — downstream handlers receive user ID as before
- `email` field is unused by all handlers (auth only used it) — `Option<String>` in `Claims` is safe to ignore downstream
- Repository trait interfaces in the domain layer
- DDD layer structure and dependency rules
- All existing non-auth tests
