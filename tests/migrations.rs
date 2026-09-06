mod test_support;

use std::borrow::Cow;

use moneykeeper::infrastructure::database::DATABASE_MIGRATOR;
use moneykeeper::infrastructure::test_db::FreshDatabase;
use sqlx::{Executor, PgPool};

async fn fresh_database() -> FreshDatabase {
    test_support::fresh_database().await
}

#[allow(clippy::too_many_arguments)]
async fn insert_legacy_journal(
    pool: &PgPool,
    id: uuid::Uuid,
    user_id: uuid::Uuid,
    asset_account_id: uuid::Uuid,
    expense_account_id: uuid::Uuid,
    command_name: &str,
    source: &str,
    purpose: &str,
    description: &str,
    idempotency_key: &str,
) {
    let mut transaction = pool.begin().await.unwrap();
    sqlx::query(
        "INSERT INTO ledger.journal_entries( \
         id,user_id,command_name,source,purpose,description,actor_kind,occurred_at,recorded_at, \
         correlation_id,idempotency_key) \
         VALUES($1,$2,$3,$4,$5,$6,'system','2026-08-01T12:00:00Z', \
                '2026-08-01T12:00:01Z',$7,$8)",
    )
    .bind(id)
    .bind(user_id)
    .bind(command_name)
    .bind(source)
    .bind(purpose)
    .bind(description)
    .bind(uuid::Uuid::new_v4())
    .bind(idempotency_key)
    .execute(&mut *transaction)
    .await
    .unwrap();
    for (account_id, account_nature, position, amount) in [
        (asset_account_id, "asset", 1_i16, "-10"),
        (expense_account_id, "expense", 2_i16, "10"),
    ] {
        sqlx::query(
            "INSERT INTO ledger.postings( \
             id,journal_entry_id,user_id,account_id,currency,account_nature,position,signed_amount) \
             VALUES($1,$2,$3,$4,'UAH',$5,$6,$7::ledger.numeric_28_8)",
        )
        .bind(uuid::Uuid::new_v4())
        .bind(id)
        .bind(user_id)
        .bind(account_id)
        .bind(account_nature)
        .bind(position)
        .bind(amount)
        .execute(&mut *transaction)
        .await
        .unwrap();
    }
    transaction.commit().await.unwrap();
}

async fn assert_rejected_before_migrations(database: &FreshDatabase) {
    let error = database
        .initialize()
        .await
        .expect_err("unmarked non-empty database must be rejected");
    assert!(
        format!("{error:#}").contains("refusing non-Moneykeeper database"),
        "unexpected error: {error:#}"
    );

    let pool = PgPool::connect(database.database_url()).await.unwrap();
    let migration_history: Option<String> =
        sqlx::query_scalar("SELECT to_regclass('_sqlx_migrations')::text")
            .fetch_one(&pool)
            .await
            .unwrap();
    let lineage_marker: Option<String> =
        sqlx::query_scalar("SELECT to_regclass('shared_kernel.database_lineage')::text")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(migration_history.is_none());
    assert!(lineage_marker.is_none());
}

#[tokio::test]
async fn database_generation_empty_database_migrates_to_complete_baseline() {
    let database = fresh_database().await;
    database
        .initialize()
        .await
        .expect("initialize empty Moneykeeper database");
}

#[tokio::test]
async fn database_generation_complete_baseline_reopens_idempotently() {
    let database = fresh_database().await;
    database
        .initialize()
        .await
        .expect("initialize Moneykeeper database");
    database
        .initialize()
        .await
        .expect("reopen marked Moneykeeper database");
}

#[tokio::test]
async fn unknown_or_modified_sqlx_history_is_rejected_by_the_migrator() {
    for (tamper, expected_error) in [
        (
            "INSERT INTO _sqlx_migrations \
             (version, description, success, checksum, execution_time) \
             VALUES (9999, 'unknown migration', TRUE, decode('00', 'hex'), 0)",
            "migration 9999 was previously applied but is missing",
        ),
        (
            "UPDATE _sqlx_migrations SET checksum = decode('00', 'hex') WHERE version = 1",
            "migration 1 was previously applied but has been modified",
        ),
    ] {
        let database = fresh_database().await;
        database.initialize().await.unwrap();
        let pool = PgPool::connect(database.database_url()).await.unwrap();
        sqlx::query(tamper).execute(&pool).await.unwrap();
        pool.close().await;

        let error = database
            .initialize()
            .await
            .expect_err("tampered SQLx history must not produce a verified pool");
        let message = format!("{error:#}");
        assert!(message.contains("run Moneykeeper migrations"), "{message}");
        assert!(message.contains(expected_error), "{message}");
    }
}

#[tokio::test]
async fn nonempty_unmarked_database_is_rejected_before_migrations_run() {
    let database = fresh_database().await;
    let pool = PgPool::connect(database.database_url()).await.unwrap();
    pool.execute("CREATE TABLE public.unrelated_data (id BIGINT PRIMARY KEY)")
        .await
        .unwrap();
    pool.close().await;

    let error = database
        .initialize()
        .await
        .expect_err("arbitrary non-empty database must be rejected");
    assert!(
        format!("{error:#}").contains("refusing non-Moneykeeper database"),
        "unexpected error: {error:#}"
    );

    let pool = PgPool::connect(database.database_url()).await.unwrap();
    let migration_history: Option<String> =
        sqlx::query_scalar("SELECT to_regclass('_sqlx_migrations')::text")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(migration_history.is_none());
}

#[tokio::test]
async fn empty_custom_schema_is_rejected_before_migrations_run() {
    let database = fresh_database().await;
    let pool = PgPool::connect(database.database_url()).await.unwrap();
    pool.execute("CREATE SCHEMA arbitrary_application")
        .await
        .unwrap();
    pool.close().await;

    assert_rejected_before_migrations(&database).await;
}

#[tokio::test]
async fn public_types_routines_and_procedures_are_rejected_before_migrations_run() {
    for ddl in [
        "CREATE TYPE public.arbitrary_state AS ENUM ('new')",
        "CREATE FUNCTION public.arbitrary_function() RETURNS INTEGER \
         LANGUAGE SQL AS 'SELECT 1'",
        "CREATE PROCEDURE public.arbitrary_procedure() \
         LANGUAGE plpgsql AS 'BEGIN NULL; END'",
    ] {
        let database = fresh_database().await;
        let pool = PgPool::connect(database.database_url()).await.unwrap();
        pool.execute(ddl).await.unwrap();
        pool.close().await;

        assert_rejected_before_migrations(&database).await;
    }
}

#[tokio::test]
async fn nondefault_extension_is_rejected_before_migrations_run() {
    let database = fresh_database().await;
    let pool = PgPool::connect(database.database_url()).await.unwrap();
    pool.execute("CREATE EXTENSION hstore")
        .await
        .expect("PostgreSQL 16 test image must provide hstore");
    pool.close().await;

    assert_rejected_before_migrations(&database).await;
}

#[tokio::test]
async fn empty_database_is_initialized_with_stable_lineage() {
    let database = fresh_database().await;
    let verified = database.initialize().await.unwrap();
    let mut connection = verified.acquire().await.unwrap();

    let marker: String =
        sqlx::query_scalar("SELECT lineage FROM shared_kernel.database_lineage WHERE singleton")
            .fetch_one(&mut *connection)
            .await
            .unwrap();
    assert_eq!(marker, "finance-v2");
}

#[tokio::test]
async fn already_marked_database_is_reopened_idempotently() {
    let database = fresh_database().await;
    database.initialize().await.unwrap();
    let verified = database.initialize().await.unwrap();
    let mut connection = verified.acquire().await.unwrap();

    let marker_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM shared_kernel.database_lineage")
            .fetch_one(&mut *connection)
            .await
            .unwrap();
    assert_eq!(marker_count, 1);
}

#[tokio::test]
async fn database_generation_wrong_lineage_marker_is_rejected_before_migration() {
    let database = fresh_database().await;
    let pool = PgPool::connect(database.database_url()).await.unwrap();
    sqlx::query("CREATE SCHEMA shared_kernel")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("CREATE TABLE shared_kernel.database_lineage (singleton BOOLEAN, lineage TEXT)")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO shared_kernel.database_lineage (singleton, lineage) VALUES (TRUE, 'legacy')",
    )
    .execute(&pool)
    .await
    .unwrap();

    pool.close().await;

    let error = database
        .initialize()
        .await
        .expect_err("wrong marker must be rejected");
    assert!(
        format!("{error:#}").contains("invalid Moneykeeper lineage marker"),
        "unexpected error: {error:#}"
    );

    let pool = PgPool::connect(database.database_url()).await.unwrap();
    let migration_history: Option<String> =
        sqlx::query_scalar("SELECT to_regclass('_sqlx_migrations')::text")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(migration_history.is_none());
}

#[tokio::test]
async fn database_generation_partial_lineage_resumes_to_the_embedded_baseline() {
    let database = fresh_database().await;
    let pool = PgPool::connect(database.database_url()).await.unwrap();
    let partial = sqlx::migrate::Migrator {
        migrations: Cow::Owned(DATABASE_MIGRATOR.iter().take(4).cloned().collect()),
        ..sqlx::migrate::Migrator::DEFAULT
    };
    partial.run(&pool).await.unwrap();
    let before: Vec<i64> =
        sqlx::query_scalar("SELECT version FROM _sqlx_migrations ORDER BY version")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(before, vec![1, 2, 3, 4]);
    pool.close().await;

    let verified = database
        .initialize()
        .await
        .expect("marked partial Moneykeeper lineage should resume");
    let mut connection = verified.acquire().await.unwrap();
    let after: Vec<i64> =
        sqlx::query_scalar("SELECT version FROM _sqlx_migrations ORDER BY version")
            .fetch_all(&mut *connection)
            .await
            .unwrap();
    let expected: Vec<i64> = DATABASE_MIGRATOR
        .iter()
        .filter(|migration| migration.migration_type.is_up_migration())
        .map(|migration| migration.version)
        .collect();
    assert_eq!(after, expected);
}

#[tokio::test]
async fn category_classification_migrations_preserve_intent_and_repair_projections() {
    let database = fresh_database().await;
    let pool = PgPool::connect(database.database_url()).await.unwrap();
    let before_categories = sqlx::migrate::Migrator {
        migrations: Cow::Owned(
            DATABASE_MIGRATOR
                .iter()
                .filter(|migration| migration.version < 17)
                .cloned()
                .collect(),
        ),
        ..sqlx::migrate::Migrator::DEFAULT
    };
    before_categories.run(&pool).await.unwrap();

    let user_id = uuid::Uuid::new_v4();
    let food_id = uuid::Uuid::new_v4();
    let archived_id = uuid::Uuid::new_v4();
    sqlx::query(
        "INSERT INTO classification.categories(id,user_id,name,kind,lifecycle,version) \
         VALUES($1,$3,'Food','expense','active',7),($2,$3,'Old','expense','archived',4)",
    )
    .bind(food_id)
    .bind(archived_id)
    .bind(user_id)
    .execute(&pool)
    .await
    .unwrap();

    let asset_account_id = uuid::Uuid::new_v4();
    let expense_account_id = uuid::Uuid::new_v4();
    sqlx::query(
        "INSERT INTO ledger.accounts( \
         id,user_id,name,currency,nature,kind,authority,visibility,system_role) \
         VALUES($1,$3,'Checking','UAH','asset','cash','manual','user_visible',NULL), \
               ($2,$3,'Uncategorized expense','UAH','expense','system','system','hidden', \
                'uncategorized_expense')",
    )
    .bind(asset_account_id)
    .bind(expense_account_id)
    .bind(user_id)
    .execute(&pool)
    .await
    .unwrap();

    let manual_id = uuid::Uuid::new_v4();
    let recurring_id = uuid::Uuid::new_v4();
    let ambiguous_recurring_id = uuid::Uuid::new_v4();
    let clear_id = uuid::Uuid::new_v4();
    let provider_import_id = uuid::Uuid::new_v4();
    let manual_missing_id = uuid::Uuid::new_v4();
    let unrelated_import_id = uuid::Uuid::new_v4();
    for (id, command, source, description, key) in [
        (
            manual_id,
            "record_manual_transaction",
            "manual",
            "Manual assigned",
            "legacy-manual",
        ),
        (
            recurring_id,
            "record_manual_transaction",
            "manual",
            "Recurring assigned",
            "legacy-recurring",
        ),
        (
            ambiguous_recurring_id,
            "record_manual_transaction",
            "manual",
            "Ambiguous recurring",
            "legacy-ambiguous-recurring",
        ),
        (
            clear_id,
            "record_manual_transaction",
            "manual",
            "Legacy clear",
            "legacy-clear",
        ),
        (
            provider_import_id,
            "import_provider_transaction",
            "import",
            "Provider import",
            "legacy-provider-import",
        ),
        (
            manual_missing_id,
            "record_manual_transaction",
            "manual",
            "Manual without annotation",
            "legacy-manual-missing",
        ),
        (
            unrelated_import_id,
            "other_import",
            "import",
            "Unrelated import",
            "legacy-other-import",
        ),
    ] {
        insert_legacy_journal(
            &pool,
            id,
            user_id,
            asset_account_id,
            expense_account_id,
            command,
            source,
            "ordinary",
            description,
            key,
        )
        .await;
    }

    for (journal_id, category_id, version, description) in [
        (manual_id, Some(food_id), 3_i64, "Manual assigned"),
        (recurring_id, Some(food_id), 4_i64, "Recurring assigned"),
        (
            ambiguous_recurring_id,
            Some(food_id),
            5_i64,
            "Ambiguous recurring",
        ),
        (clear_id, None, 2_i64, "Legacy clear"),
    ] {
        sqlx::query(
            "INSERT INTO ledger.transaction_annotations( \
             id,journal_entry_id,user_id,description,category_id,version) \
             VALUES($1,$1,$2,$3,$4,$5)",
        )
        .bind(journal_id)
        .bind(user_id)
        .bind(description)
        .bind(category_id)
        .bind(version)
        .execute(&pool)
        .await
        .unwrap();
    }

    let evidence_id = uuid::Uuid::new_v4();
    let source_evidence_id = uuid::Uuid::new_v4();
    sqlx::query(
        "INSERT INTO recurring.charge_evidence( \
         id,user_id,source_context,source_evidence_id,kind,merchant,amount,currency,charged_at,recorded_at) \
         VALUES($1,$2,'migration-test',$3,'renewal','Cafe',10,'UAH', \
                '2026-08-01T12:00:00Z','2026-08-01T12:00:01Z')",
    )
    .bind(evidence_id)
    .bind(user_id)
    .bind(source_evidence_id)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO recurring.charge_matching( \
         evidence_id,user_id,version,allocated_amount,state,updated_at) \
         VALUES($1,$2,2,20,'matched','2026-08-01T12:00:01Z')",
    )
    .bind(evidence_id)
    .bind(user_id)
    .execute(&pool)
    .await
    .unwrap();
    for (journal_id, produced_version) in [(recurring_id, 4_i64), (ambiguous_recurring_id, 4)] {
        let match_id = uuid::Uuid::new_v4();
        sqlx::query(
            "INSERT INTO recurring.match_records( \
             id,user_id,evidence_id,matching_version,decision_source,category_id,created_at) \
             VALUES($1,$2,$3,$4,'migration-test',$5,'2026-08-01T12:00:01Z')",
        )
        .bind(match_id)
        .bind(user_id)
        .bind(evidence_id)
        .bind(if journal_id == recurring_id {
            1_i64
        } else {
            2_i64
        })
        .bind(food_id)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO recurring.match_allocations(match_id,user_id,journal_entry_id,amount,currency) \
             VALUES($1,$2,$3,10,'UAH')",
        )
        .bind(match_id)
        .bind(user_id)
        .bind(journal_id)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO recurring.categorization_targets( \
             match_id,user_id,journal_entry_id,state,process_generation,produced_annotation_version,updated_at) \
             VALUES($1,$2,$3,'posted',1,$4,'2026-08-01T12:00:01Z')",
        )
        .bind(match_id)
        .bind(user_id)
        .bind(journal_id)
        .bind(produced_version)
        .execute(&pool)
        .await
        .unwrap();
    }

    sqlx::query(
        "INSERT INTO reporting.cashflows( \
         user_id,journal_entry_id,flow_kind,amount,currency,category_id,effective_at,source_sequence) \
         VALUES($1,$2,'expense',10,'UAH',$3,'2026-08-01T12:00:00Z',10), \
               ($1,$4,'expense',5,'UAH',$3,'2026-08-01T12:00:00Z',11)",
    )
    .bind(user_id)
    .bind(manual_id)
    .bind(archived_id)
    .bind(clear_id)
    .execute(&pool)
    .await
    .unwrap();
    pool.close().await;

    let verified = database.initialize().await.unwrap();
    let mut connection = verified.acquire().await.unwrap();

    let taxonomy: (i64, Option<i32>) = sqlx::query_as(
        "SELECT version,starter_template_version FROM classification.category_taxonomies \
         WHERE user_id=$1",
    )
    .bind(user_id)
    .fetch_one(&mut *connection)
    .await
    .unwrap();
    assert_eq!(taxonomy, (1, None));
    let migrated_categories: Vec<(uuid::Uuid, Option<uuid::Uuid>, i32, String, i64)> =
        sqlx::query_as(
            "SELECT id,parent_id,position,lifecycle,version FROM classification.categories \
             WHERE user_id=$1 ORDER BY position",
        )
        .bind(user_id)
        .fetch_all(&mut *connection)
        .await
        .unwrap();
    assert_eq!(migrated_categories.len(), 2);
    assert_eq!(migrated_categories[0].1, None);
    assert_eq!(migrated_categories[1].1, None);
    assert_eq!(migrated_categories[0].2, 0);
    assert_eq!(migrated_categories[1].2, 1);
    assert!(
        migrated_categories
            .iter()
            .any(|row| { row.0 == food_id && row.3 == "active" && row.4 == 7 })
    );
    assert!(
        migrated_categories
            .iter()
            .any(|row| { row.0 == archived_id && row.3 == "archived" && row.4 == 4 })
    );

    for (journal_id, expected) in [
        (manual_id, (Some(food_id), Some("manual"), "suppressed")),
        (recurring_id, (Some(food_id), Some("recurring"), "eligible")),
        (
            ambiguous_recurring_id,
            (Some(food_id), Some("manual"), "suppressed"),
        ),
        (clear_id, (None, None, "legacy_unknown")),
        (provider_import_id, (None, None, "eligible")),
    ] {
        let actual: (Option<uuid::Uuid>, Option<String>, String) = sqlx::query_as(
            "SELECT category_id,assignment_origin,automation_state \
             FROM ledger.transaction_annotations WHERE user_id=$1 AND journal_entry_id=$2",
        )
        .bind(user_id)
        .bind(journal_id)
        .fetch_one(&mut *connection)
        .await
        .unwrap();
        assert_eq!((actual.0, actual.1.as_deref(), actual.2.as_str()), expected);
    }
    let generated_id: uuid::Uuid = sqlx::query_scalar(
        "SELECT id FROM ledger.transaction_annotations WHERE user_id=$1 AND journal_entry_id=$2",
    )
    .bind(user_id)
    .bind(provider_import_id)
    .fetch_one(&mut *connection)
    .await
    .unwrap();
    assert_eq!(generated_id, provider_import_id);
    let unrelated_annotations: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM ledger.transaction_annotations \
         WHERE user_id=$1 AND journal_entry_id IN ($2,$3)",
    )
    .bind(user_id)
    .bind(manual_missing_id)
    .bind(unrelated_import_id)
    .fetch_one(&mut *connection)
    .await
    .unwrap();
    assert_eq!(unrelated_annotations, 0);

    let repaired: Vec<(uuid::Uuid, Option<uuid::Uuid>, i64)> = sqlx::query_as(
        "SELECT journal_entry_id,category_id,category_annotation_version \
         FROM reporting.cashflows WHERE user_id=$1 ORDER BY source_sequence",
    )
    .bind(user_id)
    .fetch_all(&mut *connection)
    .await
    .unwrap();
    assert_eq!(
        repaired,
        vec![(manual_id, Some(food_id), 3), (clear_id, None, 2)]
    );
}

#[tokio::test]
async fn split_event_consumer_migration_seeds_both_receipts_and_retains_history() {
    let database = fresh_database().await;
    let pool = PgPool::connect(database.database_url()).await.unwrap();
    let before_split = sqlx::migrate::Migrator {
        migrations: Cow::Owned(
            DATABASE_MIGRATOR
                .iter()
                .filter(|migration| migration.version < 12)
                .cloned()
                .collect(),
        ),
        ..sqlx::migrate::Migrator::DEFAULT
    };
    before_split.run(&pool).await.unwrap();

    let message_id = uuid::Uuid::new_v4();
    sqlx::query(
        "INSERT INTO integration.inbox_receipts \
         (consumer_name, message_id, event_type, received_at, processed_at) \
         VALUES ('finance-v2-phase4-router', $1, 'ledger.journal-posted.v1', \
                 '2026-08-23T10:00:00Z', '2026-08-23T10:00:01Z')",
    )
    .bind(message_id)
    .execute(&pool)
    .await
    .unwrap();
    pool.close().await;

    let verified = database.initialize().await.unwrap();
    let mut connection = verified.acquire().await.unwrap();
    let consumers: Vec<String> = sqlx::query_scalar(
        "SELECT consumer_name FROM integration.inbox_receipts \
         WHERE message_id = $1 ORDER BY consumer_name",
    )
    .bind(message_id)
    .fetch_all(&mut *connection)
    .await
    .unwrap();
    assert_eq!(
        consumers,
        vec![
            "finance-v2-phase4-router",
            "recurring-event-policy-v1",
            "reporting-projections-v1",
        ]
    );

    let split = DATABASE_MIGRATOR
        .iter()
        .find(|migration| migration.version == 12)
        .expect("migration 0012 must remain embedded");
    assert_eq!(split.description, "split event consumer receipts");
}

#[tokio::test]
async fn monobank_worker_migration_backfills_replayable_state_additively() {
    let database = fresh_database().await;
    let pool = PgPool::connect(database.database_url()).await.unwrap();
    let before_workers = sqlx::migrate::Migrator {
        migrations: Cow::Owned(
            DATABASE_MIGRATOR
                .iter()
                .filter(|migration| migration.version < 13)
                .cloned()
                .collect(),
        ),
        ..sqlx::migrate::Migrator::DEFAULT
    };
    before_workers.run(&pool).await.unwrap();

    let user_id = uuid::Uuid::new_v4();
    let connection_id = uuid::Uuid::new_v4();
    sqlx::query(
        "INSERT INTO banking.provider_connections
         (id,user_id,provider,state,active_credential_ciphertext,
          active_credential_nonce,active_credential_key_id,active_credential_envelope_version)
         VALUES ($1,$2,'monobank','pending',$3,$4,'legacy-key',1)",
    )
    .bind(connection_id)
    .bind(user_id)
    .bind(vec![1_u8])
    .bind(vec![2_u8; 12])
    .execute(&pool)
    .await
    .unwrap();
    let receipt_id = uuid::Uuid::new_v4();
    sqlx::query(
        "INSERT INTO banking.webhook_receipts
         (id,user_id,connection_id,delivery_digest,state)
         VALUES ($1,$2,$3,$4,'pending')",
    )
    .bind(receipt_id)
    .bind(user_id)
    .bind(connection_id)
    .bind(vec![3_u8; 32])
    .execute(&pool)
    .await
    .unwrap();
    pool.close().await;

    let verified = database.initialize().await.unwrap();
    let mut connection = verified.acquire().await.unwrap();
    let validation: (String, i32, Option<chrono::DateTime<chrono::Utc>>) = sqlx::query_as(
        "SELECT validation_state,validation_attempts,validation_next_retry_at
         FROM banking.provider_connections WHERE id=$1 AND user_id=$2",
    )
    .bind(connection_id)
    .bind(user_id)
    .fetch_one(&mut *connection)
    .await
    .unwrap();
    assert_eq!(validation, ("pending".to_owned(), 0, None));
    let receipt: (String, Option<String>) = sqlx::query_as(
        "SELECT state,last_error FROM banking.webhook_receipts WHERE id=$1 AND user_id=$2",
    )
    .bind(receipt_id)
    .bind(user_id)
    .fetch_one(&mut *connection)
    .await
    .unwrap();
    assert_eq!(receipt.0, "quarantined");
    assert!(receipt.1.is_some_and(|error| error.len() <= 500));
    let worker_tables: Vec<String> = sqlx::query_scalar(
        "SELECT table_name FROM information_schema.tables
         WHERE table_schema='banking'
           AND table_name IN ('sync_job_resources','sync_page_events')
         ORDER BY table_name",
    )
    .fetch_all(&mut *connection)
    .await
    .unwrap();
    assert_eq!(
        worker_tables,
        vec!["sync_job_resources", "sync_page_events"]
    );
    let worker_constraints: Vec<String> = sqlx::query_scalar(
        "SELECT conname FROM pg_constraint
         WHERE connamespace='banking'::regnamespace AND conname IN (
           'provider_connection_validation_lease',
           'provider_connection_webhook_lease',
           'webhook_receipt_provenance_complete',
           'sync_page_statement_identity'
         ) ORDER BY conname",
    )
    .fetch_all(&mut *connection)
    .await
    .unwrap();
    assert_eq!(
        worker_constraints,
        vec![
            "provider_connection_validation_lease",
            "provider_connection_webhook_lease",
            "sync_page_statement_identity",
            "webhook_receipt_provenance_complete",
        ]
    );

    // The older binary omits every 0013 column. Defaults keep that INSERT
    // compatible during rollback/redeploy rehearsal.
    sqlx::query(
        "INSERT INTO banking.provider_connections (id,user_id,provider,state)
         VALUES ($1,$2,'monobank','pending')",
    )
    .bind(uuid::Uuid::new_v4())
    .bind(user_id)
    .execute(&mut *connection)
    .await
    .unwrap();
}

#[tokio::test]
async fn resource_specific_sync_migration_preserves_and_classifies_legacy_jobs() {
    let database = fresh_database().await;
    let pool = PgPool::connect(database.database_url()).await.unwrap();
    let before_resource_targets = sqlx::migrate::Migrator {
        migrations: Cow::Owned(
            DATABASE_MIGRATOR
                .iter()
                .filter(|migration| migration.version < 15)
                .cloned()
                .collect(),
        ),
        ..sqlx::migrate::Migrator::DEFAULT
    };
    before_resource_targets.run(&pool).await.unwrap();

    let user_id = uuid::Uuid::new_v4();
    let connection_id = uuid::Uuid::new_v4();
    sqlx::query(
        "INSERT INTO banking.provider_connections (id,user_id,provider,state)
         VALUES ($1,$2,'monobank','active')",
    )
    .bind(connection_id)
    .bind(user_id)
    .execute(&pool)
    .await
    .unwrap();
    let resource_a = uuid::Uuid::new_v4();
    let resource_b = uuid::Uuid::new_v4();
    for (id, external) in [(resource_a, "migration-a"), (resource_b, "migration-b")] {
        sqlx::query(
            "INSERT INTO banking.external_resources
             (id,user_id,connection_id,external_resource_id,kind,funding_model,currency,masked_label)
             VALUES ($1,$2,$3,$4,'card','own_funds','UAH',$4)",
        )
        .bind(id)
        .bind(user_id)
        .bind(connection_id)
        .bind(external)
        .execute(&pool)
        .await
        .unwrap();
    }
    let single_job = uuid::Uuid::new_v4();
    let multi_job = uuid::Uuid::new_v4();
    for job_id in [single_job, multi_job] {
        sqlx::query(
            "INSERT INTO banking.sync_jobs
             (id,user_id,connection_id,requested_from,requested_to,state,
              connection_version,credential_generation)
             VALUES ($1,$2,$3,'2026-08-01T00:00:00Z','2026-08-02T00:00:00Z',
                     'requested',1,1)",
        )
        .bind(job_id)
        .bind(user_id)
        .bind(connection_id)
        .execute(&pool)
        .await
        .unwrap();
    }
    for (job_id, resource_id, position) in [
        (single_job, resource_a, 1_i32),
        (multi_job, resource_a, 1_i32),
        (multi_job, resource_b, 2_i32),
    ] {
        sqlx::query(
            "INSERT INTO banking.sync_job_resources
             (sync_job_id,user_id,connection_id,external_resource_id,position,
              snapshot_from,snapshot_to,next_from)
             VALUES ($1,$2,$3,$4,$5,'2026-08-01T00:00:00Z',
                     '2026-08-02T00:00:00Z','2026-08-01T00:00:00Z')",
        )
        .bind(job_id)
        .bind(user_id)
        .bind(connection_id)
        .bind(resource_id)
        .bind(position)
        .execute(&pool)
        .await
        .unwrap();
    }
    pool.close().await;

    let verified = database.initialize().await.unwrap();
    let mut connection = verified.acquire().await.unwrap();
    let targets: Vec<(uuid::Uuid, Option<uuid::Uuid>)> = sqlx::query_as(
        "SELECT id,resource_id FROM banking.sync_jobs
         WHERE id IN ($1,$2) ORDER BY id",
    )
    .bind(single_job)
    .bind(multi_job)
    .fetch_all(&mut *connection)
    .await
    .unwrap();
    let target_for = |id| targets.iter().find(|row| row.0 == id).unwrap().1;
    assert_eq!(target_for(single_job), Some(resource_a));
    assert_eq!(target_for(multi_job), None);
    let legacy_resources: i64 =
        sqlx::query_scalar("SELECT count(*) FROM banking.sync_job_resources WHERE sync_job_id=$1")
            .bind(multi_job)
            .fetch_one(&mut *connection)
            .await
            .unwrap();
    assert_eq!(legacy_resources, 2);

    let targeted_job = uuid::Uuid::new_v4();
    sqlx::query(
        "INSERT INTO banking.sync_jobs
         (id,user_id,connection_id,resource_id,requested_from,requested_to,state,
          connection_version,credential_generation)
         VALUES ($1,$2,$3,$4,'2026-08-01T00:00:00Z','2026-08-02T00:00:00Z',
                 'requested',1,1)",
    )
    .bind(targeted_job)
    .bind(user_id)
    .bind(connection_id)
    .bind(resource_a)
    .execute(&mut *connection)
    .await
    .unwrap();
    let mismatched_snapshot = sqlx::query(
        "INSERT INTO banking.sync_job_resources
         (sync_job_id,user_id,connection_id,external_resource_id,position,
          snapshot_from,snapshot_to,next_from)
         VALUES ($1,$2,$3,$4,1,'2026-08-01T00:00:00Z',
                 '2026-08-02T00:00:00Z','2026-08-01T00:00:00Z')",
    )
    .bind(targeted_job)
    .bind(user_id)
    .bind(connection_id)
    .bind(resource_b)
    .execute(&mut *connection)
    .await;
    assert!(mismatched_snapshot.is_err());
}

#[tokio::test]
async fn database_generation_failure_returns_no_pool_and_redacts_database_password() {
    let database = fresh_database().await;
    let pool = PgPool::connect(database.database_url()).await.unwrap();
    let root_only = sqlx::migrate::Migrator {
        migrations: Cow::Owned(DATABASE_MIGRATOR.iter().take(1).cloned().collect()),
        ..sqlx::migrate::Migrator::DEFAULT
    };
    root_only.run(&pool).await.unwrap();
    sqlx::query("CREATE TABLE integration.outbox_messages (conflict BOOLEAN)")
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;

    let error = database
        .initialize()
        .await
        .expect_err("conflicting partial database must not produce a verified pool");
    let message = format!("{error:#}");
    assert!(message.contains("run Moneykeeper migrations"), "{message}");
    assert!(!message.contains("postgres:postgres"), "{message}");
    assert!(!message.contains(database.database_url()), "{message}");

    let pool = PgPool::connect(database.database_url()).await.unwrap();
    let versions: Vec<i64> =
        sqlx::query_scalar("SELECT version FROM _sqlx_migrations ORDER BY version")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(versions, vec![1]);
}

#[tokio::test]
async fn root_migration_creates_owned_schemas_and_no_legacy_tables() {
    let database = fresh_database().await;
    let verified = database.initialize().await.unwrap();
    let mut connection = verified.acquire().await.unwrap();
    let schemas: Vec<String> = sqlx::query_scalar(
        r#"
        SELECT schema_name
        FROM information_schema.schemata
        WHERE schema_name = ANY($1)
        ORDER BY schema_name
        "#,
    )
    .bind(
        &[
            "shared_kernel",
            "reference_data",
            "classification",
            "preferences",
            "integration",
            "ledger",
            "banking",
            "mail",
            "recurring",
            "reporting",
            "sharing",
            "loans",
            "portfolio",
        ][..],
    )
    .fetch_all(&mut *connection)
    .await
    .unwrap();

    assert_eq!(schemas.len(), 13);
    for legacy_table in ["accounts", "transactions", "bank_connections"] {
        let relation: Option<String> =
            sqlx::query_scalar("SELECT to_regclass(format('public.%I', $1))::text")
                .bind(legacy_table)
                .fetch_one(&mut *connection)
                .await
                .unwrap();
        assert!(relation.is_none(), "legacy table {legacy_table} exists");
    }
}

#[tokio::test]
async fn root_migration_seeds_and_constrains_reference_and_tenant_data() {
    let database = fresh_database().await;
    let verified = database.initialize().await.unwrap();
    let mut connection = verified.acquire().await.unwrap();

    let currencies: Vec<String> = sqlx::query_scalar(
        "SELECT code FROM reference_data.currencies WHERE enabled ORDER BY code",
    )
    .fetch_all(&mut *connection)
    .await
    .unwrap();
    assert!(currencies.iter().any(|code| code == "UAH"));
    assert!(currencies.iter().any(|code| code == "USD"));
    assert!(currencies.iter().any(|code| code == "EUR"));

    let popular: Vec<(String, String, String, i16, bool)> = sqlx::query_as(
        "SELECT code, numeric_code, name, minor_unit, enabled \
         FROM reference_data.currencies \
         WHERE code NOT IN ('UAH', 'USD', 'EUR') ORDER BY code",
    )
    .fetch_all(&mut *connection)
    .await
    .unwrap();
    assert_eq!(
        popular,
        vec![
            (
                "AED".to_owned(),
                "784".to_owned(),
                "UAE Dirham".to_owned(),
                2,
                true
            ),
            (
                "AUD".to_owned(),
                "036".to_owned(),
                "Australian Dollar".to_owned(),
                2,
                true
            ),
            (
                "CAD".to_owned(),
                "124".to_owned(),
                "Canadian Dollar".to_owned(),
                2,
                true
            ),
            (
                "CHF".to_owned(),
                "756".to_owned(),
                "Swiss Franc".to_owned(),
                2,
                true
            ),
            (
                "CNY".to_owned(),
                "156".to_owned(),
                "Yuan Renminbi".to_owned(),
                2,
                true
            ),
            (
                "CZK".to_owned(),
                "203".to_owned(),
                "Czech Koruna".to_owned(),
                2,
                true
            ),
            (
                "GBP".to_owned(),
                "826".to_owned(),
                "Pound Sterling".to_owned(),
                2,
                true
            ),
            (
                "GEL".to_owned(),
                "981".to_owned(),
                "Lari".to_owned(),
                2,
                true
            ),
            (
                "HUF".to_owned(),
                "348".to_owned(),
                "Forint".to_owned(),
                2,
                true
            ),
            (
                "ILS".to_owned(),
                "376".to_owned(),
                "New Israeli Sheqel".to_owned(),
                2,
                true
            ),
            (
                "JPY".to_owned(),
                "392".to_owned(),
                "Yen".to_owned(),
                0,
                true
            ),
            (
                "PLN".to_owned(),
                "985".to_owned(),
                "Zloty".to_owned(),
                2,
                true
            ),
            (
                "RON".to_owned(),
                "946".to_owned(),
                "Romanian Leu".to_owned(),
                2,
                true
            ),
            (
                "RUB".to_owned(),
                "643".to_owned(),
                "Russian Ruble".to_owned(),
                2,
                false
            ),
            (
                "TRY".to_owned(),
                "949".to_owned(),
                "Turkish Lira".to_owned(),
                2,
                true
            ),
        ]
    );

    for (code, minor_unit) in [("usd", 2_i16), ("US", 2), ("USDX", 2), ("ZZZ", 9)] {
        let result = sqlx::query(
            "INSERT INTO reference_data.currencies \
             (code, name, minor_unit, enabled) VALUES ($1, 'Invalid', $2, TRUE)",
        )
        .bind(code)
        .bind(minor_unit)
        .execute(&mut *connection)
        .await;
        assert!(
            result.is_err(),
            "invalid currency {code}/{minor_unit} accepted"
        );
    }

    let user_id = uuid::Uuid::new_v4();
    let category_id = uuid::Uuid::new_v4();
    sqlx::query(
        "INSERT INTO classification.categories \
         (id, user_id, name, kind, position) VALUES ($1, $2, 'Food', 'expense', 0)",
    )
    .bind(category_id)
    .bind(user_id)
    .execute(&mut *connection)
    .await
    .unwrap();
    let duplicate = sqlx::query(
        "INSERT INTO classification.categories \
         (id, user_id, name, kind, position) VALUES ($1, $2, 'fOoD', 'expense', 1)",
    )
    .bind(uuid::Uuid::new_v4())
    .bind(user_id)
    .execute(&mut *connection)
    .await;
    assert!(duplicate.is_err());

    for (name, kind, lifecycle, version) in [
        ("Bad kind".to_owned(), "asset", "active", 1_i64),
        ("Bad lifecycle".to_owned(), "expense", "deleted", 1),
        ("Bad version".to_owned(), "expense", "active", 0),
        ("x".repeat(101), "expense", "active", 1),
    ] {
        let invalid_category = sqlx::query(
            "INSERT INTO classification.categories \
             (id, user_id, name, kind, lifecycle, version, position) \
             VALUES ($1, $2, $3, $4, $5, $6, 1)",
        )
        .bind(uuid::Uuid::new_v4())
        .bind(user_id)
        .bind(&name)
        .bind(kind)
        .bind(lifecycle)
        .bind(version)
        .execute(&mut *connection)
        .await;
        assert!(
            invalid_category.is_err(),
            "invalid category {name:?}/{kind}/{lifecycle}/{version} accepted"
        );
    }

    let missing_currency = sqlx::query(
        "INSERT INTO preferences.user_preferences (user_id, base_currency) \
         VALUES ($1, 'ZZZ')",
    )
    .bind(user_id)
    .execute(&mut *connection)
    .await;
    assert!(missing_currency.is_err());

    sqlx::query("UPDATE reference_data.currencies SET enabled = FALSE WHERE code = 'EUR'")
        .execute(&mut *connection)
        .await
        .unwrap();
    let disabled_currency = sqlx::query(
        "INSERT INTO preferences.user_preferences (user_id, base_currency) \
         VALUES ($1, 'EUR')",
    )
    .bind(uuid::Uuid::new_v4())
    .execute(&mut *connection)
    .await;
    assert!(disabled_currency.is_err());

    let invalid_preference_version = sqlx::query(
        "INSERT INTO preferences.user_preferences (user_id, base_currency, version) \
         VALUES ($1, 'UAH', 0)",
    )
    .bind(uuid::Uuid::new_v4())
    .execute(&mut *connection)
    .await;
    assert!(invalid_preference_version.is_err());
}

#[tokio::test]
async fn banking_migration_installs_the_owned_storage_baseline() {
    let database = fresh_database().await;
    let verified = database.initialize().await.unwrap();
    let mut connection = verified.acquire().await.unwrap();
    for table in [
        "provider_connections",
        "external_resources",
        "resource_mappings",
        "provider_events",
        "balance_observations",
        "sync_jobs",
        "sync_pages",
        "command_receipts",
    ] {
        let relation: Option<String> =
            sqlx::query_scalar("SELECT to_regclass(format('banking.%I', $1))::text")
                .bind(table)
                .fetch_one(&mut *connection)
                .await
                .unwrap();
        assert!(relation.is_some(), "missing banking.{table}");
    }
}

#[tokio::test]
async fn database_lineage_cannot_be_updated_or_deleted() {
    let database = fresh_database().await;
    let verified = database.initialize().await.unwrap();
    let mut connection = verified.acquire().await.unwrap();

    let update =
        sqlx::query("UPDATE shared_kernel.database_lineage SET lineage = 'legacy' WHERE singleton")
            .execute(&mut *connection)
            .await;
    let delete = sqlx::query("DELETE FROM shared_kernel.database_lineage WHERE singleton")
        .execute(&mut *connection)
        .await;
    assert!(update.is_err());
    assert!(delete.is_err());
}

#[tokio::test]
async fn third_migration_installs_the_strict_ledger_baseline() {
    let database = fresh_database().await;
    let verified = database.initialize().await.unwrap();
    let mut connection = verified.acquire().await.unwrap();

    let applied: Vec<i64> =
        sqlx::query_scalar("SELECT version FROM _sqlx_migrations WHERE success ORDER BY version")
            .fetch_all(&mut *connection)
            .await
            .unwrap();
    assert_eq!(&applied[..3], &[1, 2, 3]);

    for relation in [
        "ledger.accounts",
        "ledger.journal_entries",
        "ledger.postings",
        "ledger.account_balances",
        "ledger.command_receipts",
        "ledger.audit_events",
    ] {
        let installed: Option<String> = sqlx::query_scalar("SELECT to_regclass($1)::text")
            .bind(relation)
            .fetch_one(&mut *connection)
            .await
            .unwrap();
        assert_eq!(installed.as_deref(), Some(relation));
    }
}

#[tokio::test]
async fn feature_context_migrations_install_owned_storage() {
    let database = fresh_database().await;
    let verified = database.initialize().await.unwrap();
    let mut connection = verified.acquire().await.unwrap();
    for relation in [
        "mail.connections",
        "mail.command_receipts",
        "mail.source_messages",
        "recurring.subscriptions",
        "recurring.charge_matching",
        "recurring.match_allocations",
        "reference_data.fx_observations",
        "reference_data.fx_sync_state",
        "reporting.consumed_events",
        "reporting.account_balances",
        "reporting.bill_positions",
        "reporting.loan_summaries",
        "reporting.portfolio_valuations",
    ] {
        let installed: Option<String> = sqlx::query_scalar("SELECT to_regclass($1)::text")
            .bind(relation)
            .fetch_one(&mut *connection)
            .await
            .unwrap();
        assert_eq!(installed.as_deref(), Some(relation), "missing {relation}");
    }
}
