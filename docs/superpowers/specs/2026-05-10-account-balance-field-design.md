# Account Balance Field Design

## Context

Currently, account balances are computed on-demand by aggregating all transactions for an account (`GET /accounts/{id}/balance`). This is a separate endpoint requiring an extra round-trip. The goal is to embed a `balance` field directly in every account response (both list and single-get), computed from the `accounts` table itself — making balance reads O(1) and eliminating the dedicated balance endpoint.

## Approach: Materialized Balance

Store `balance NUMERIC NOT NULL DEFAULT 0` on the `accounts` table. Every transaction mutation (create, update, delete) adjusts the owning account's balance by the signed delta. Reads are free — no aggregation.

Existing accounts start at `balance = 0` (migration default); only future transactions move it.

---

## Layer-by-Layer Changes

### 1. DB Migration

```sql
ALTER TABLE accounts ADD COLUMN balance NUMERIC NOT NULL DEFAULT 0;
```

### 2. Domain — `src/domain/account.rs`

**`Account` struct** — add field:
```rust
pub balance: Decimal,
```

**`AccountRepository` trait**:
- Remove: `async fn compute_balance(&self, account_id: Uuid, user_id: Uuid) -> Result<Decimal>`
- Add: `async fn adjust_balance(&self, account_id: Uuid, user_id: Uuid, delta: Decimal) -> Result<()>`

### 3. Infrastructure — `src/infrastructure/account_repository.rs`

- `AccountRow` struct: add `balance: Decimal` field
- All `SELECT` queries: include `balance` column
- `Account::from(row)` (or wherever mapping happens): map `balance`
- Remove `compute_balance` implementation
- Add `adjust_balance` implementation:
  ```sql
  UPDATE accounts SET balance = balance + $1 WHERE id = $2 AND user_id = $3
  ```

### 4. Application — `src/application/transactions.rs`

`TransactionService` gains `Arc<dyn AccountRepository>` as a constructor dependency.

Signed delta helper (reuses `TransactionKind::affects_balance_positively()`):
```rust
fn signed_delta(kind: &TransactionKind, amount: Decimal) -> Decimal {
    if kind.affects_balance_positively() { amount } else { -amount }
}
```

**`create()`**: after inserting the transaction, call:
```rust
account_repo.adjust_balance(transaction.account_id, transaction.user_id, signed_delta(&kind, amount)).await?;
```

**`delete()`**: fetch the transaction first to get its kind and amount, then:
```rust
account_repo.adjust_balance(account_id, user_id, -signed_delta(&old.kind, old.amount)).await?;
```

**`update()`**: fetch old transaction, compute:
```rust
let delta = signed_delta(&new_kind, new_amount) - signed_delta(&old.kind, old.amount);
account_repo.adjust_balance(account_id, user_id, delta).await?;
```

> **Edge case**: if `account_id` can change during update (transaction moves between accounts), the old account must be debited by `-old_signed` and the new account credited by `+new_signed`. If account_id is immutable during updates, a single adjust_balance call suffices.

### 5. Application — `src/application/accounts.rs`

- Remove `get_balance(id, user_id) -> Result<Decimal>` method (balance now lives on `Account`)

### 6. Infrastructure — Monobank Sync

`MonobankService` currently uses `Arc<dyn TransactionRepository>` directly for bulk upserts. Change it to receive `Arc<TransactionService>` and route all transaction creation through `TransactionService.create()`. This ensures balance updates happen automatically during sync.

Wiring change in `src/main.rs`: pass `Arc<transaction_service>` to `MonobankService` instead of the raw transaction repo.

### 7. API — `src/api/dto.rs`

- `AccountResponse`: add `balance: Decimal`
- Remove `BalanceResponse` struct

### 8. API — `src/api/handlers/accounts.rs`

- Remove `get_balance` handler function

### 9. API — `src/api/routes.rs`

- Remove `GET /accounts/{id}/balance` route

### 10. OpenAPI — `static/openapi.json`

- `AccountResponse` schema: add `"balance"` to `required` and `properties` (type: number)
- Remove `/accounts/{id}/balance` path entry
- Remove `BalanceResponse` schema

---

## Critical Files

| File | Change |
|------|--------|
| `src/domain/account.rs` | Add `balance` to `Account`; swap `compute_balance` → `adjust_balance` on trait |
| `src/infrastructure/account_repository.rs` | Update queries, `AccountRow`, impl `adjust_balance`, remove `compute_balance` |
| `src/application/transactions.rs` | Add account repo dep; call `adjust_balance` in create/update/delete |
| `src/application/accounts.rs` | Remove `get_balance` |
| `src/infrastructure/monobank/` | Accept `TransactionService` instead of raw repo |
| `src/api/dto.rs` | Add `balance` to `AccountResponse`; remove `BalanceResponse` |
| `src/api/handlers/accounts.rs` | Remove `get_balance` handler |
| `src/api/routes.rs` | Remove `/accounts/{id}/balance` route |
| `static/openapi.json` | Update `AccountResponse`; remove balance path and schema |
| `migrations/` | New migration file |

---

## Verification

1. `cargo build` — no compile errors
2. `cargo clippy` — no warnings
3. `cargo test` — all tests pass
4. Manually: create an account → verify `balance: 0` in response
5. Add an income transaction → GET /accounts/{id} → verify balance increased
6. Add an expense → verify balance decreased
7. Delete the expense → verify balance reverts
8. Update a transaction amount → verify balance reflects the delta
9. Trigger a Monobank sync → verify balances update correctly
10. Confirm `GET /accounts/{id}/balance` returns 404 (route removed)
