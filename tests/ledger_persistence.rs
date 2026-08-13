use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

#[path = "v2_test_support.rs"]
mod v2_test_support;

async fn account(pool: &PgPool, user: Uuid, currency: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO ledger.accounts \
         (id, user_id, name, currency, nature, kind, authority, visibility, lifecycle) \
         VALUES ($1, $2, 'Test', $3, 'asset', 'cash', 'manual', 'user_visible', 'active')",
    )
    .bind(id)
    .bind(user)
    .bind(currency)
    .execute(pool)
    .await
    .unwrap();
    id
}

async fn journal(tx: &mut Transaction<'_, Postgres>, user: Uuid, key: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO ledger.journal_entries \
         (id, user_id, command_name, source, purpose, description, actor_kind, \
          occurred_at, recorded_at, correlation_id, idempotency_key) \
         VALUES ($1, $2, 'test', 'manual', 'ordinary', 'Test', 'user', \
                 clock_timestamp(), clock_timestamp(), $3, $4)",
    )
    .bind(id)
    .bind(user)
    .bind(Uuid::new_v4())
    .bind(key)
    .execute(&mut **tx)
    .await
    .unwrap();
    id
}

async fn posting(
    tx: &mut Transaction<'_, Postgres>,
    journal_id: Uuid,
    user: Uuid,
    account_id: Uuid,
    currency: &str,
    position: i16,
    amount: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO ledger.postings \
         (id, journal_entry_id, user_id, account_id, currency, account_nature, position, signed_amount) \
         VALUES ($1, $2, $3, $4, $5, 'asset', $6, $7::numeric)",
    )
    .bind(Uuid::new_v4())
    .bind(journal_id)
    .bind(user)
    .bind(account_id)
    .bind(currency)
    .bind(position)
    .bind(amount)
    .execute(&mut **tx)
    .await
    .map(|_| ())
}

#[tokio::test]
async fn schema_rejects_unbalanced_cross_tenant_wrong_currency_and_mutation() {
    let (_verified, pool) = v2_test_support::fresh_v2_runtime().await;
    let user = Uuid::new_v4();
    let other = Uuid::new_v4();
    let cash = account(&pool, user, "UAH").await;
    let savings = account(&pool, user, "UAH").await;
    let other_cash = account(&pool, other, "UAH").await;
    let usd = account(&pool, user, "USD").await;

    let mut one = pool.begin().await.unwrap();
    let one_id = journal(&mut one, user, "one").await;
    posting(&mut one, one_id, user, cash, "UAH", 1, "10.00")
        .await
        .unwrap();
    assert!(one.commit().await.is_err(), "one-posting journal committed");

    let mut unbalanced = pool.begin().await.unwrap();
    let unbalanced_id = journal(&mut unbalanced, user, "unbalanced").await;
    posting(&mut unbalanced, unbalanced_id, user, cash, "UAH", 1, "10.00").await.unwrap();
    posting(&mut unbalanced, unbalanced_id, user, savings, "UAH", 2, "-9.00").await.unwrap();
    assert!(unbalanced.commit().await.is_err(), "unbalanced journal committed");

    let mut tenant = pool.begin().await.unwrap();
    let tenant_id = journal(&mut tenant, user, "tenant").await;
    assert!(posting(&mut tenant, tenant_id, user, other_cash, "UAH", 1, "10.00").await.is_err());
    tenant.rollback().await.unwrap();

    let mut currency = pool.begin().await.unwrap();
    let currency_id = journal(&mut currency, user, "currency").await;
    assert!(posting(&mut currency, currency_id, user, usd, "UAH", 1, "10.00").await.is_err());
    currency.rollback().await.unwrap();

    let mut balanced = pool.begin().await.unwrap();
    let balanced_id = journal(&mut balanced, user, "balanced").await;
    posting(&mut balanced, balanced_id, user, cash, "UAH", 1, "10.00").await.unwrap();
    posting(&mut balanced, balanced_id, user, savings, "UAH", 2, "-10.00").await.unwrap();
    balanced.commit().await.unwrap();

    assert!(sqlx::query("UPDATE ledger.journal_entries SET description = 'tampered' WHERE id = $1")
        .bind(balanced_id).execute(&pool).await.is_err());
    assert!(sqlx::query("DELETE FROM ledger.journal_entries WHERE id = $1")
        .bind(balanced_id).execute(&pool).await.is_err());
    assert!(sqlx::query("UPDATE ledger.postings SET signed_amount = 99 WHERE journal_entry_id = $1")
        .bind(balanced_id).execute(&pool).await.is_err());
    assert!(sqlx::query("DELETE FROM ledger.postings WHERE journal_entry_id = $1")
        .bind(balanced_id).execute(&pool).await.is_err());
}

#[tokio::test]
async fn schema_accepts_balanced_multi_currency_and_freezes_account_currency() {
    let (_verified, pool) = v2_test_support::fresh_v2_runtime().await;
    let user = Uuid::new_v4();
    let uah_a = account(&pool, user, "UAH").await;
    let uah_b = account(&pool, user, "UAH").await;
    let usd_a = account(&pool, user, "USD").await;
    let usd_b = account(&pool, user, "USD").await;

    assert!(sqlx::query("UPDATE ledger.accounts SET currency = 'EUR' WHERE id = $1 AND user_id = $2")
        .bind(uah_a).bind(user).execute(&pool).await.is_err());

    let mut tx = pool.begin().await.unwrap();
    let id = journal(&mut tx, user, "fx").await;
    posting(&mut tx, id, user, uah_a, "UAH", 1, "-4000.00").await.unwrap();
    posting(&mut tx, id, user, uah_b, "UAH", 2, "4000.00").await.unwrap();
    posting(&mut tx, id, user, usd_a, "USD", 3, "100.00").await.unwrap();
    posting(&mut tx, id, user, usd_b, "USD", 4, "-100.00").await.unwrap();
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn schema_constrains_idempotency_numeric_bounds_reversals_and_reconciliation() {
    let (_verified, pool) = v2_test_support::fresh_v2_runtime().await;
    let user = Uuid::new_v4();
    let cash = account(&pool, user, "UAH").await;
    let savings = account(&pool, user, "UAH").await;

    sqlx::query(
        "INSERT INTO ledger.command_receipts \
         (user_id, command_name, idempotency_key, request_hash, status, result, completed_at) \
         VALUES ($1, 'expense', 'same', decode(repeat('00', 32), 'hex'), 'completed', '{}'::jsonb, clock_timestamp())",
    ).bind(user).execute(&pool).await.unwrap();
    assert!(sqlx::query(
        "INSERT INTO ledger.command_receipts \
         (user_id, command_name, idempotency_key, request_hash, status, result, completed_at) \
         VALUES ($1, 'expense', 'same', decode(repeat('11', 32), 'hex'), 'completed', '{}'::jsonb, clock_timestamp())",
    ).bind(user).execute(&pool).await.is_err());

    assert!(sqlx::query(
        "INSERT INTO ledger.account_balances \
         (account_id, user_id, currency, signed_balance, version) \
         VALUES ($1, $2, 'UAH', 1.000000001, 1)",
    ).bind(cash).bind(user).execute(&pool).await.is_err());
    assert!(sqlx::query(
        "INSERT INTO ledger.account_balances \
         (account_id, user_id, currency, signed_balance, version) \
         VALUES ($1, $2, 'UAH', 100000000000000000000.00, 1)",
    ).bind(cash).bind(user).execute(&pool).await.is_err());

    let mut original = pool.begin().await.unwrap();
    let original_id = journal(&mut original, user, "original").await;
    posting(&mut original, original_id, user, cash, "UAH", 1, "1.00").await.unwrap();
    posting(&mut original, original_id, user, savings, "UAH", 2, "-1.00").await.unwrap();
    original.commit().await.unwrap();

    for key in ["reversal-1", "reversal-2"] {
        let mut reversal = pool.begin().await.unwrap();
        let id = Uuid::new_v4();
        let insert = sqlx::query(
            "INSERT INTO ledger.journal_entries \
             (id, user_id, command_name, source, purpose, description, actor_kind, occurred_at, recorded_at, \
              correlation_id, idempotency_key, reverses_transaction_id) \
             VALUES ($1, $2, 'reverse', 'correction', 'reversal', 'Reverse', 'user', \
                     clock_timestamp(), clock_timestamp(), $3, $4, $5)",
        ).bind(id).bind(user).bind(Uuid::new_v4()).bind(key).bind(original_id)
          .execute(&mut *reversal).await;
        if key == "reversal-2" {
            assert!(insert.is_err(), "duplicate reversal relation accepted");
            reversal.rollback().await.unwrap();
            break;
        }
        insert.unwrap();
        posting(&mut reversal, id, user, cash, "UAH", 1, "-1.00").await.unwrap();
        posting(&mut reversal, id, user, savings, "UAH", 2, "1.00").await.unwrap();
        reversal.commit().await.unwrap();
    }

    assert!(sqlx::query(
        "INSERT INTO ledger.reconciliation_cases \
         (id, user_id, account_id, observation_id, source_kind, source_stream_id, source_item_id, \
          observed_at, source_sequence, recorded_at, provider_reported_balance, currency, \
          captured_ledger_balance, captured_balance_version, delta, status, version, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, 'feed', 'stream', 'item', clock_timestamp(), 1, clock_timestamp(), \
                 10, 'UAH', 0, 1, 10, 'impossible', 0, clock_timestamp(), clock_timestamp())",
    ).bind(Uuid::new_v4()).bind(user).bind(cash).bind(Uuid::new_v4())
      .execute(&pool).await.is_err());
}
