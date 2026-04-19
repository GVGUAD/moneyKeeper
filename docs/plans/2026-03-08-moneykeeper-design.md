# MoneyKeeper — Backend Design

**Date:** 2026-03-08

## Overview

A personal finance tracking REST API backend in Rust. Tracks income and expenses across cash, bank, savings, loan, investment (stocks/ETFs), and Binance (crypto) accounts. Multi-user with JWT auth. SQLite database.

---

## Tech Stack

| Crate | Purpose |
|---|---|
| `axum` | HTTP framework (async, built on tokio) |
| `sqlx` | SQLite driver with async + compile-time query checking |
| `jsonwebtoken` | JWT encode/decode |
| `argon2` | Password hashing |
| `uuid` | UUIDs for all IDs |
| `serde` + `serde_json` | JSON serialization |
| `chrono` | Timestamps |
| `rust_decimal` | Precise money arithmetic (no floating point) |
| `tower-http` | Middleware (CORS, request tracing) |
| `validator` | Request payload validation |

---

## Auth

- Register with email + password (Argon2 hashed)
- Login returns JWT access token (short-lived, ~15min) + refresh token (long-lived, ~30 days)
- Refresh tokens stored hashed in `refresh_tokens` table
- All endpoints require `Authorization: Bearer <token>`
- All resources scoped to `user_id`

**Schema:**
```sql
users: id, email, password_hash, created_at
refresh_tokens: id, user_id, token_hash, expires_at, created_at
```

**Endpoints:**
```
POST /auth/register
POST /auth/login
POST /auth/refresh
POST /auth/logout
```

---

## Accounts

Base table + extension tables per type (Option C — base + extensions).

**Schema:**
```sql
accounts: id, user_id, name, account_type, currency, created_at, updated_at

-- account_type: Cash | Bank | Savings | Loan | Investment | Binance

savings_details: account_id, interest_rate, compounding_period
-- compounding_period: Daily | Monthly | Quarterly | Annually

loan_details: account_id, counterparty, direction, interest_rate (nullable), due_date (nullable)
-- direction: Borrowed | Lent

investment_details: account_id, broker (nullable)

binance_details: account_id, label (nullable)
```

- Cash and Bank have no extension table
- Account balance is always computed from transactions (no stored balance field)

**Endpoints:**
```
POST   /accounts
GET    /accounts
GET    /accounts/:id
PUT    /accounts/:id
DELETE /accounts/:id
GET    /accounts/:id/balance
```

---

## Transactions

Base table + extension tables per kind.

**Schema:**
```sql
transactions: id, account_id, user_id, amount (Decimal, always positive),
              currency, kind, category_id (nullable), note (nullable),
              transacted_at, created_at

-- kind: Income | Expense | Transfer | Buy | Sell | StakingReward

transfer_links: from_transaction_id, to_transaction_id
-- Transfer creates two transactions (debit + credit) linked here

trade_details: transaction_id, ticker, quantity, price_per_unit (nullable), fee (nullable)
-- Used for Buy, Sell, StakingReward (price_per_unit is null for staking rewards)
```

**Account type usage:**

| Account | Typical transaction kinds |
|---|---|
| Cash / Bank / Savings | Income, Expense, Transfer |
| Loan | Income (disbursement), Expense (repayment) |
| Investment | Buy, Sell, Income (dividend) |
| Binance | Buy, Sell, StakingReward, Transfer |

**Endpoints:**
```
POST   /accounts/:id/transactions
GET    /accounts/:id/transactions   (paginated, filterable by date/kind/category)
GET    /transactions/:id
PUT    /transactions/:id
DELETE /transactions/:id
GET    /transactions                (all accounts, paginated)
```

---

## Categories

User-defined flat list. Any transaction can have one optional category.

**Schema:**
```sql
categories: id, user_id, name, color (nullable hex), created_at
```

- Deleting a category nullifies `category_id` on existing transactions (no cascade delete)

**Endpoints:**
```
POST   /categories
GET    /categories
PUT    /categories/:id
DELETE /categories/:id
```

---

## Error Handling

- `domain/` — `thiserror` enums (e.g. `AccountNotFound`, `InvalidAmount`, `InsufficientPermission`)
- `application/` — `anyhow::Result` propagation
- HTTP handlers — map domain errors to HTTP status codes with JSON body `{ "error": "..." }`

---

## Project Structure

```
src/
  domain/
    account.rs           # Account entity + extension value objects
    transaction.rs       # Transaction entity + extension value objects
    category.rs          # Category entity
    user.rs              # User entity
    error.rs             # Domain error enums
    repository/          # Repository traits

  application/
    auth/                # RegisterUseCase, LoginUseCase, RefreshUseCase, LogoutUseCase
    accounts/            # CreateAccountUseCase, GetAccountUseCase, UpdateAccountUseCase, DeleteAccountUseCase, GetBalanceUseCase
    transactions/        # CreateTransactionUseCase, ListTransactionsUseCase, UpdateTransactionUseCase, DeleteTransactionUseCase
    categories/          # CreateCategoryUseCase, ListCategoriesUseCase, UpdateCategoryUseCase, DeleteCategoryUseCase

  infrastructure/
    db/                  # Sqlx repository implementations
    migrations/          # SQLx migration files (.sql)

  api/
    routes.rs            # Axum router wiring
    handlers/            # HTTP handler functions
    middleware/          # JWT auth middleware
    dto/                 # Request/response structs (serde)

  main.rs                # Wire everything together
```
