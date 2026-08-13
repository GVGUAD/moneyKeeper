use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;
use chrono::{TimeZone, Utc};
use moneykeeper::contexts::ledger::public::{
    AccountKind, AccountLifecycle, AccountNature, AccountVersion, ArchiveAccount, OpenAccount,
    RenameAccount, RestoreAccount,
};
use moneykeeper::shared_kernel::{
    CorrelationId, CurrencyCode, IdempotencyKey, Money, UserId,
};
use rust_decimal::Decimal;

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

#[test]
fn unit_of_work_contract_is_transaction_bound_and_write_only() {
    let ports = include_str!("../src/contexts/ledger/application/ports.rs");
    let adapter = include_str!("../src/contexts/ledger/infrastructure/pg_unit_of_work.rs");
    assert!(ports.contains("trait LedgerUnitOfWork"));
    assert!(ports.contains("type Tx<'a>"));
    assert!(adapter.contains("Transaction<'a, Postgres>"));
    assert!(!adapter.contains("set_balance"));
    assert!(!adapter.contains("delete_journal"));
}

fn open_command(user_id: UserId, key: &str, name: &str, balance: &str, kind: AccountKind, nature: AccountNature) -> OpenAccount {
    let currency = CurrencyCode::new("UAH").unwrap();
    OpenAccount {
        user_id,
        name: name.to_owned(),
        currency: currency.clone(),
        kind,
        nature,
        opening_balance: Money::new(Decimal::from_str_exact(balance).unwrap(), currency, 2).unwrap(),
        idempotency_key: IdempotencyKey::new(key).unwrap(),
        correlation_id: CorrelationId::generate(),
        causation_id: None,
        occurred_at: Utc.with_ymd_and_hms(2026, 8, 5, 10, 0, 0).unwrap(),
    }
}

#[tokio::test]
async fn account_command_posts_opening_balances_and_replays_exactly() {
    let (verified, pool) = v2_test_support::fresh_v2_runtime().await;
    let ledger = moneykeeper::contexts::ledger::build(&verified);
    let user = UserId::generate();

    let asset = ledger.open_account(open_command(
        user, "asset-open", "Cash", "125.00", AccountKind::Cash, AccountNature::Asset,
    )).await.unwrap();
    assert_eq!(asset.account.signed_balance, Decimal::new(12500, 2));
    assert_eq!(asset.account.display_balance, Decimal::new(12500, 2));
    assert!(asset.opening_journal_id.is_some());
    assert!(!asset.replayed);

    let replay = ledger.open_account(open_command(
        user, "asset-open", "Cash", "125.00", AccountKind::Cash, AccountNature::Asset,
    )).await.unwrap();
    assert_eq!(replay.account.id, asset.account.id);
    assert_eq!(replay.opening_journal_id, asset.opening_journal_id);
    assert!(replay.replayed);

    let conflict = ledger.open_account(open_command(
        user, "asset-open", "Different", "125.00", AccountKind::Cash, AccountNature::Asset,
    )).await.unwrap_err();
    assert!(conflict.is_idempotency_conflict());

    let liability = ledger.open_account(open_command(
        user, "liability-open", "Card", "80.00", AccountKind::CreditCard, AccountNature::Liability,
    )).await.unwrap();
    assert_eq!(liability.account.signed_balance, Decimal::new(-8000, 2));
    assert_eq!(liability.account.display_balance, Decimal::new(8000, 2));

    let negative = ledger.open_account(open_command(
        user, "negative-open", "Overdraft cash", "-10.00", AccountKind::Cash, AccountNature::Asset,
    )).await.unwrap();
    assert_eq!(negative.account.display_balance, Decimal::new(-1000, 2));

    let zero = ledger.open_account(open_command(
        user, "zero-open", "Empty", "0.00", AccountKind::Savings, AccountNature::Asset,
    )).await.unwrap();
    assert!(zero.opening_journal_id.is_none());

    let mismatches: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM ledger.account_balances b \
         WHERE b.user_id = $1 AND b.signed_balance <> COALESCE(( \
             SELECT SUM(p.signed_amount) FROM ledger.postings p \
             WHERE p.user_id = b.user_id AND p.account_id = b.account_id), 0)",
    ).bind(user.into_uuid()).fetch_one(&pool).await.unwrap();
    assert_eq!(mismatches, 0);
}

#[tokio::test]
async fn account_command_versions_and_tenant_scope_metadata_changes() {
    let (verified, _pool) = v2_test_support::fresh_v2_runtime().await;
    let ledger = moneykeeper::contexts::ledger::build(&verified);
    let user = UserId::generate();
    let opened = ledger.open_account(open_command(
        user, "metadata-open", "Cash", "25.00", AccountKind::Cash, AccountNature::Asset,
    )).await.unwrap();

    let renamed = ledger.rename_account(RenameAccount {
        user_id: user,
        account_id: opened.account.id,
        name: "Wallet".to_owned(),
        expected_version: AccountVersion::INITIAL,
        idempotency_key: IdempotencyKey::new("rename-1").unwrap(),
        correlation_id: CorrelationId::generate(),
        occurred_at: Utc.with_ymd_and_hms(2026, 8, 5, 11, 0, 0).unwrap(),
    }).await.unwrap();
    assert_eq!(renamed.account.name, "Wallet");
    assert_eq!(renamed.account.display_balance, Decimal::new(2500, 2));

    let stale = ledger.rename_account(RenameAccount {
        user_id: user,
        account_id: opened.account.id,
        name: "Stale".to_owned(),
        expected_version: AccountVersion::INITIAL,
        idempotency_key: IdempotencyKey::new("rename-stale").unwrap(),
        correlation_id: CorrelationId::generate(),
        occurred_at: Utc::now(),
    }).await.unwrap_err();
    assert!(stale.is_version_conflict());

    let invisible = ledger.rename_account(RenameAccount {
        user_id: UserId::generate(),
        account_id: opened.account.id,
        name: "Foreign".to_owned(),
        expected_version: renamed.account.version,
        idempotency_key: IdempotencyKey::new("rename-foreign").unwrap(),
        correlation_id: CorrelationId::generate(),
        occurred_at: Utc::now(),
    }).await.unwrap_err();
    assert!(invisible.is_not_found());

    let archived = ledger.archive_account(ArchiveAccount {
        user_id: user,
        account_id: opened.account.id,
        expected_version: renamed.account.version,
        idempotency_key: IdempotencyKey::new("archive-1").unwrap(),
        correlation_id: CorrelationId::generate(),
        occurred_at: Utc::now(),
    }).await.unwrap();
    assert_eq!(archived.account.lifecycle, AccountLifecycle::Archived);
    assert_eq!(archived.account.display_balance, Decimal::new(2500, 2));

    let restored = ledger.restore_account(RestoreAccount {
        user_id: user,
        account_id: opened.account.id,
        expected_version: archived.account.version,
        idempotency_key: IdempotencyKey::new("restore-1").unwrap(),
        correlation_id: CorrelationId::generate(),
        occurred_at: Utc::now(),
    }).await.unwrap();
    assert_eq!(restored.account.lifecycle, AccountLifecycle::Active);
    assert_eq!(restored.account.display_balance, Decimal::new(2500, 2));
}

#[tokio::test]
async fn unit_of_work_rolls_back_every_financial_stage() {
    for (table, operation) in [
        ("journal_entries", "INSERT"),
        ("postings", "INSERT"),
        ("account_balances", "UPDATE"),
        ("command_receipts", "INSERT"),
        ("audit_events", "INSERT"),
        ("integration.outbox_messages", "INSERT"),
    ] {
        let (verified, pool) = v2_test_support::fresh_v2_runtime().await;
        let ledger = moneykeeper::contexts::ledger::build(&verified);
        sqlx::query(
            "CREATE FUNCTION ledger.fail_test_write() RETURNS TRIGGER LANGUAGE plpgsql AS $$ \
             BEGIN RAISE EXCEPTION 'injected failure'; END; $$",
        ).execute(&pool).await.unwrap();
        let qualified = if table.contains('.') { table.to_owned() } else { format!("ledger.{table}") };
        let trigger = format!(
            "CREATE TRIGGER injected_failure BEFORE {operation} ON {qualified} \
             FOR EACH ROW EXECUTE FUNCTION ledger.fail_test_write()"
        );
        sqlx::query(&trigger).execute(&pool).await.unwrap();

        let result = ledger.open_account(open_command(
            UserId::generate(), &format!("fail-{table}-{operation}"), "Atomic", "10.00",
            AccountKind::Cash, AccountNature::Asset,
        )).await;
        assert!(result.is_err(), "{operation} on {table} did not fail");

        for relation in [
            "ledger.accounts", "ledger.journal_entries", "ledger.postings",
            "ledger.account_balances", "ledger.command_receipts", "ledger.audit_events",
            "integration.outbox_messages",
        ] {
            let count: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM {relation}"))
                .fetch_one(&pool).await.unwrap();
            assert_eq!(count, 0, "{relation} survived failure at {table}/{operation}");
        }
    }
}
