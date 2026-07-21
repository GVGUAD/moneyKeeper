use std::borrow::Cow;

use sqlx::postgres::PgDatabaseError;
use sqlx::{PgPool, Row};
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, ImageExt};
use testcontainers_modules::postgres::Postgres;
use uuid::Uuid;

use moneykeeper::domain::bank_connection::BankConnectionRepository;
use moneykeeper::domain::email_connection::EmailConnectionRepository;
use moneykeeper::domain::subscription::SubscriptionRepository;
use moneykeeper::domain::subscription_charge::SubscriptionChargeRepository;
use moneykeeper::infrastructure::email_connection_repository::PgEmailConnectionRepository;
use moneykeeper::infrastructure::monobank_repository::PgBankConnectionRepository;
use moneykeeper::infrastructure::subscription_charge_repository::PgSubscriptionChargeRepository;
use moneykeeper::infrastructure::subscription_repository::PgSubscriptionRepository;

static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("src/infrastructure/migrations");
const LATEST_MIGRATION: i64 = 25;
const DEPLOYED_0011_VERSIONS: &[i64] = &[1, 2, 3, 4, 5, 6, 7, 9, 10, 11];
const DEPLOYED_CHECKSUMS: &[(i64, &str)] = &[
    (
        1,
        "7873211efc61bf3f7166fa2d9f3c2d0e1549fd5e30c6c330b9aa26d17b2c17be7eb67c496a368e435746494648ec757d",
    ),
    (
        2,
        "51821ef3e49254943f2595758a30f178d1e65e21854eb7600aeca6dcf6e4291f5335c136df095eca18ef338838dfc44c",
    ),
    (
        3,
        "354ddcbd6031264100ae7415f8e49d174304ec811c3a3ddf4d1eeed650817886b66ce7a85c52cb46731f0a3b14d686fd",
    ),
    (
        4,
        "fb89c12d371b6e7291f2e21fe63343f80b0a7094de687c9f1001c205db92fa5f2a432aa8949c831d1e748d5c2b2d3e66",
    ),
    (
        5,
        "22fe524a0944bfd3c60175fb276a4459af04ba935285d18fff46f7e4ade4aabdfe7aa4a779b58b9aff201da10ec5f670",
    ),
    (
        6,
        "d3fddfc39485f240d63fd5862010c6fff3bbe72e942c976516eb54e340cb88cb15edc3647d0b47bf11efd1e9d52b8f0e",
    ),
    (
        7,
        "389d2a136937f213a09d72dfabe39d45075fc5f8828bef9647bfe0263f983e09957152e4d62d09390be0cb7303815a77",
    ),
    (
        9,
        "85ecb9b463e5a375f20c1346ff6648c0fa0072e5ffa1952450654755869f236705bf4cbc656f1b9cbb334a07f6f26aba",
    ),
    (
        10,
        "aaecafddc013c339be27494c018bad7a4784aefb5cef6392acc89c3ce46fdb362e7cfd38c053c1b11bc087a9957b6653",
    ),
    (
        11,
        "631c6e95aef23d7b38b3ddf2d0d6cf608976ff17fe705392482b0eafe522888924fffcab277a206410c86154cfcd868d",
    ),
];

struct LegacyPostgres {
    _container: ContainerAsync<Postgres>,
    pool: PgPool,
}

impl LegacyPostgres {
    async fn empty() -> Self {
        let container = Postgres::default()
            .with_tag("16-alpine")
            .start()
            .await
            .expect("failed to start PostgreSQL 16");
        let port = container
            .get_host_port_ipv4(5432)
            .await
            .expect("failed to get PostgreSQL port");
        let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");
        let pool = PgPool::connect(&url)
            .await
            .expect("failed to connect to PostgreSQL 16");

        Self {
            _container: container,
            pool,
        }
    }

    async fn at_0011() -> Self {
        let postgres = Self::empty().await;
        seed_deployed_0011_ledger(&postgres.pool).await;
        postgres
    }
}

/// Reproduce a real deployment that had versions 1-7 and 9-11 recorded before
/// the currently uncommitted version 8 existed. The latest SQLx migrator must
/// validate those checksums, apply the out-of-order additive version 8, then
/// apply every additive migration through the current integrity rollout.
async fn seed_deployed_0011_ledger(pool: &PgPool) {
    sqlx::raw_sql(
        "CREATE TABLE _sqlx_migrations (\
            version BIGINT PRIMARY KEY,\
            description TEXT NOT NULL,\
            installed_on TIMESTAMPTZ NOT NULL DEFAULT now(),\
            success BOOLEAN NOT NULL,\
            checksum BYTEA NOT NULL,\
            execution_time BIGINT NOT NULL\
        )",
    )
    .execute(pool)
    .await
    .unwrap();

    for migration in MIGRATOR
        .iter()
        .filter(|migration| DEPLOYED_0011_VERSIONS.contains(&migration.version))
    {
        let expected_checksum = DEPLOYED_CHECKSUMS
            .iter()
            .find_map(|(version, checksum)| (*version == migration.version).then_some(*checksum))
            .expect("frozen checksum for deployed migration");
        let actual_checksum = migration
            .checksum
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        assert_eq!(
            actual_checksum, expected_checksum,
            "deployed migration {} was edited in place",
            migration.version
        );
        let mut transaction = pool.begin().await.unwrap();
        sqlx::raw_sql(&migration.sql)
            .execute(&mut *transaction)
            .await
            .unwrap_or_else(|error| {
                panic!(
                    "failed to apply historical migration {}: {error}",
                    migration.version
                )
            });
        sqlx::query(
            "INSERT INTO _sqlx_migrations \
             (version,description,success,checksum,execution_time) \
             VALUES ($1,$2,true,$3,0)",
        )
        .bind(migration.version)
        .bind(migration.description.as_ref())
        .bind(migration.checksum.as_ref())
        .execute(&mut *transaction)
        .await
        .unwrap();
        transaction.commit().await.unwrap();
    }
}

async fn migrate_through(pool: &PgPool, max_version: i64) {
    let migrator = sqlx::migrate::Migrator {
        migrations: Cow::Owned(
            MIGRATOR
                .iter()
                .filter(|migration| migration.version <= max_version)
                .cloned()
                .collect(),
        ),
        ignore_missing: false,
        locking: true,
        no_tx: false,
    };
    migrator.run(pool).await.unwrap();
}

struct LegacyFixture {
    user_id: Uuid,
    other_user_id: Uuid,
    connection_id: Uuid,
    bank_connection_id: Uuid,
    subscription_id: Uuid,
    charge_id: Uuid,
    orphaned_matched_charge_id: Uuid,
    transaction_id: Uuid,
    invalid_category_id: Uuid,
}

async fn seed_representative_legacy_rows(pool: &PgPool) -> LegacyFixture {
    let fixture = LegacyFixture {
        user_id: Uuid::new_v4(),
        other_user_id: Uuid::new_v4(),
        connection_id: Uuid::new_v4(),
        bank_connection_id: Uuid::new_v4(),
        subscription_id: Uuid::new_v4(),
        charge_id: Uuid::new_v4(),
        orphaned_matched_charge_id: Uuid::new_v4(),
        transaction_id: Uuid::new_v4(),
        invalid_category_id: Uuid::new_v4(),
    };
    let account_id = Uuid::new_v4();

    sqlx::query(
        "INSERT INTO accounts
            (id, user_id, name, account_type, currency, created_at, updated_at)
         VALUES ($1, $2, 'Legacy card', 'Bank', 'UAH', now(), now())",
    )
    .bind(account_id)
    .bind(fixture.user_id)
    .execute(pool)
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO categories (id, user_id, name, created_at)
         VALUES ($1, $2, 'Another tenant category', now())",
    )
    .bind(fixture.invalid_category_id)
    .bind(fixture.other_user_id)
    .execute(pool)
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO transactions
            (id, account_id, user_id, amount, currency, kind, transacted_at, created_at)
         VALUES ($1, $2, $3, 129.99, 'UAH', 'Expense', now(), now())",
    )
    .bind(fixture.transaction_id)
    .bind(account_id)
    .bind(fixture.user_id)
    .execute(pool)
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO bank_connections
            (id, account_id, user_id, token, external_account_id, sync_status,
             created_at, provider)
         VALUES ($1, $2, $3, 'legacy-bank-secret', 'legacy-external-id',
                 'completed', 1700000000, 'monobank')",
    )
    .bind(fixture.bank_connection_id)
    .bind(account_id)
    .bind(fixture.user_id)
    .execute(pool)
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO email_connections
            (id, user_id, provider, email_address, oauth_access_token,
             oauth_refresh_token, access_token_expires_at, status, created_at)
         VALUES ($1, $2, 'gmail', '  Legacy.User@Example.COM  ',
                 'legacy-access-secret', 'legacy-refresh-secret', 1700003600,
                 'connected', 1700000000)",
    )
    .bind(fixture.connection_id)
    .bind(fixture.user_id)
    .execute(pool)
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO subscriptions
            (id, user_id, provider, product_name, merchant_key, amount, currency,
             billing_period, status, started_at, last_charged_at, next_expected_at,
             category_id, created_at)
         VALUES ($1, $2, 'netflix', 'Legacy Cloud', 'legacy-cloud', 129.99, 'UAH',
                 'monthly', 'active', 1697000000, 1700000000, 1702678400, $3,
                 1697000000)",
    )
    .bind(fixture.subscription_id)
    .bind(fixture.user_id)
    .bind(fixture.invalid_category_id)
    .execute(pool)
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO subscription_charges
            (id, subscription_id, user_id, amount, currency, charged_at,
             email_message_id, kind, transaction_id, match_status, created_at)
         VALUES ($1, $2, $3, 129.99, 'UAH', 1700000000,
                 '<legacy-receipt@example.com>', 'renewal', $4, 'Matched',
                 1700000100)",
    )
    .bind(fixture.charge_id)
    .bind(fixture.subscription_id)
    .bind(fixture.user_id)
    .bind(fixture.transaction_id)
    .execute(pool)
    .await
    .unwrap();

    // Migration 0011 used ON DELETE SET NULL and could leave this legacy
    // combination after a bank transaction was deleted.
    sqlx::query(
        "INSERT INTO subscription_charges
            (id, subscription_id, user_id, amount, currency, charged_at,
             email_message_id, kind, transaction_id, match_status, created_at)
         VALUES ($1, $2, $3, 129.99, 'UAH', 1700000200,
                 '<orphaned-match@example.com>', 'renewal', NULL, 'Matched',
                 1700000300)",
    )
    .bind(fixture.orphaned_matched_charge_id)
    .bind(fixture.subscription_id)
    .bind(fixture.user_id)
    .execute(pool)
    .await
    .unwrap();

    fixture
}

#[tokio::test]
async fn upgrades_representative_0011_data_through_latest() {
    let postgres = LegacyPostgres::at_0011().await;
    let fixture = seed_representative_legacy_rows(&postgres.pool).await;

    MIGRATOR.run(&postgres.pool).await.unwrap();

    let applied_versions: Vec<i64> =
        sqlx::query_scalar("SELECT version FROM _sqlx_migrations ORDER BY version")
            .fetch_all(&postgres.pool)
            .await
            .unwrap();
    assert_eq!(
        applied_versions,
        (1_i64..=LATEST_MIGRATION).collect::<Vec<_>>()
    );

    // Validate the upgraded values through the same mappings used by the
    // application, not only through permissive raw SQL assertions.
    let mapped_charge = PgSubscriptionChargeRepository::new(postgres.pool.clone())
        .find_by_id(fixture.charge_id, fixture.user_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(mapped_charge.transaction_id, Some(fixture.transaction_id));
    assert_eq!(mapped_charge.provider_message_id, None);
    let mapped_orphan = PgSubscriptionChargeRepository::new(postgres.pool.clone())
        .find_by_id(fixture.orphaned_matched_charge_id, fixture.user_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(mapped_orphan.transaction_id, None);
    assert_eq!(mapped_orphan.match_status.as_str(), "Pending");
    assert_eq!(mapped_orphan.match_source, None);
    let mapped_subscription = PgSubscriptionRepository::new(postgres.pool.clone())
        .find_by_id(fixture.subscription_id, fixture.user_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(mapped_subscription.category_id, None);
    let mapped_email = PgEmailConnectionRepository::new(postgres.pool.clone())
        .find_by_id(fixture.connection_id, fixture.user_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(mapped_email.email_address, "legacy.user@example.com");
    let mapped_bank = PgBankConnectionRepository::new(postgres.pool.clone())
        .find_by_id(fixture.bank_connection_id, fixture.user_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(mapped_bank.token, "legacy-bank-secret");

    let charge = sqlx::query(
        "SELECT source, source_key, source_connection_id, provider_message_id, rfc_message_id,
                match_started_at, match_source
         FROM subscription_charges WHERE id = $1",
    )
    .bind(fixture.charge_id)
    .fetch_one(&postgres.pool)
    .await
    .unwrap();
    assert_eq!(charge.get::<String, _>("source"), "gmail");
    assert_eq!(
        charge.get::<String, _>("source_key"),
        format!("legacy:{}:<legacy-receipt@example.com>", fixture.user_id)
    );
    assert_eq!(charge.get::<Option<Uuid>, _>("source_connection_id"), None);
    assert_eq!(charge.get::<Option<String>, _>("provider_message_id"), None);
    assert_eq!(
        charge.get::<Option<String>, _>("rfc_message_id").as_deref(),
        Some("<legacy-receipt@example.com>")
    );
    assert_eq!(charge.get::<i64, _>("match_started_at"), 1700000100);
    assert_eq!(
        charge.get::<Option<String>, _>("match_source").as_deref(),
        Some("automatic")
    );

    let subscription = sqlx::query(
        "SELECT category_id, product_name_override, billing_period_override,
                status_override, last_receipt_at
         FROM subscriptions WHERE id = $1",
    )
    .bind(fixture.subscription_id)
    .fetch_one(&postgres.pool)
    .await
    .unwrap();
    assert_eq!(subscription.get::<Option<Uuid>, _>("category_id"), None);
    assert_eq!(
        subscription.get::<Option<String>, _>("product_name_override"),
        None
    );
    assert_eq!(
        subscription.get::<Option<String>, _>("billing_period_override"),
        None
    );
    assert_eq!(
        subscription.get::<Option<String>, _>("status_override"),
        None
    );
    assert_eq!(subscription.get::<i64, _>("last_receipt_at"), 1700000000);

    let bank_token_encrypted: Option<String> =
        sqlx::query_scalar("SELECT token_encrypted FROM bank_connections WHERE id = $1")
            .bind(fixture.bank_connection_id)
            .fetch_one(&postgres.pool)
            .await
            .unwrap();
    assert_eq!(bank_token_encrypted, None);

    let connection = sqlx::query(
        "SELECT email_address, oauth_access_token_encrypted,
                oauth_refresh_token_encrypted, next_sync_at, sync_attempts,
                sync_lease_owner, sync_lease_expires_at
         FROM email_connections WHERE id = $1",
    )
    .bind(fixture.connection_id)
    .fetch_one(&postgres.pool)
    .await
    .unwrap();
    assert_eq!(
        connection.get::<String, _>("email_address"),
        "legacy.user@example.com"
    );
    assert_eq!(
        connection.get::<Option<String>, _>("oauth_access_token_encrypted"),
        None
    );
    assert_eq!(
        connection.get::<Option<String>, _>("oauth_refresh_token_encrypted"),
        None
    );
    assert_eq!(connection.get::<i64, _>("next_sync_at"), 0);
    assert_eq!(connection.get::<i32, _>("sync_attempts"), 0);
    assert_eq!(connection.get::<Option<Uuid>, _>("sync_lease_owner"), None);
    assert_eq!(
        connection.get::<Option<i64>, _>("sync_lease_expires_at"),
        None
    );

    let oauth_state_table: Option<String> =
        sqlx::query_scalar("SELECT to_regclass('gmail_oauth_states')::text")
            .fetch_one(&postgres.pool)
            .await
            .unwrap();
    let ingestion_table: Option<String> =
        sqlx::query_scalar("SELECT to_regclass('email_message_ingestions')::text")
            .fetch_one(&postgres.pool)
            .await
            .unwrap();
    let tombstone_table: Option<String> =
        sqlx::query_scalar("SELECT to_regclass('subscription_tombstones')::text")
            .fetch_one(&postgres.pool)
            .await
            .unwrap();
    assert_eq!(oauth_state_table.as_deref(), Some("gmail_oauth_states"));
    assert_eq!(ingestion_table.as_deref(), Some("email_message_ingestions"));
    assert_eq!(tombstone_table.as_deref(), Some("subscription_tombstones"));

    // Distinct Gmail mailboxes remain valid for one user, while normalized
    // duplicates are rejected by the new identity constraint.
    sqlx::query(
        "INSERT INTO email_connections
            (id, user_id, provider, email_address, oauth_access_token,
             oauth_refresh_token, access_token_expires_at, status, created_at)
         VALUES ($1, $2, 'gmail', 'other@example.com', 'access', 'refresh', 0,
                 'connected', 0)",
    )
    .bind(Uuid::new_v4())
    .bind(fixture.user_id)
    .execute(&postgres.pool)
    .await
    .unwrap();

    let normalized_duplicate = sqlx::query(
        "INSERT INTO email_connections
            (id, user_id, provider, email_address, oauth_access_token,
             oauth_refresh_token, access_token_expires_at, status, created_at)
         VALUES ($1, $2, 'gmail', 'legacy.user@example.com', 'access', 'refresh',
                 0, 'connected', 0)",
    )
    .bind(Uuid::new_v4())
    .bind(fixture.user_id)
    .execute(&postgres.pool)
    .await
    .unwrap_err();
    assert!(
        normalized_duplicate
            .as_database_error()
            .unwrap()
            .is_unique_violation()
    );

    // The internal email_message_id stays globally unique while the original
    // RFC Message-ID can be reused by another tenant.
    let other_subscription_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO subscriptions
            (id, user_id, provider, product_name, merchant_key, amount, currency,
             billing_period, status, started_at, last_charged_at, next_expected_at,
             created_at, last_receipt_at)
         VALUES ($1, $2, 'netflix', 'Other Cloud', 'other-cloud', 9.99, 'USD',
                 'monthly', 'active', 1700000000, 1700000000, 1702678400,
                 1700000000, 1700000000)",
    )
    .bind(other_subscription_id)
    .bind(fixture.other_user_id)
    .execute(&postgres.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO subscription_charges
            (id, subscription_id, user_id, amount, currency, charged_at,
             email_message_id, kind, match_status, created_at, source_key,
             rfc_message_id, match_started_at)
         VALUES ($1, $2, $3, 9.99, 'USD', 1700000000,
                 $4, 'new_subscription', 'Pending',
                 1700000100, $4, '<legacy-receipt@example.com>', 1700000100)",
    )
    .bind(Uuid::new_v4())
    .bind(other_subscription_id)
    .bind(fixture.other_user_id)
    .bind(format!("gmail:{}:provider-message-2", Uuid::new_v4()))
    .execute(&postgres.pool)
    .await
    .unwrap();

    // The transaction-delete trigger restores a linked charge to a fresh
    // Pending state instead of leaving an inconsistent Matched row.
    sqlx::query("DELETE FROM transactions WHERE id = $1")
        .bind(fixture.transaction_id)
        .execute(&postgres.pool)
        .await
        .unwrap();
    let reset_charge = sqlx::query(
        "SELECT transaction_id, match_status, match_source, match_started_at
         FROM subscription_charges WHERE id = $1",
    )
    .bind(fixture.charge_id)
    .fetch_one(&postgres.pool)
    .await
    .unwrap();
    assert_eq!(reset_charge.get::<Option<Uuid>, _>("transaction_id"), None);
    assert_eq!(reset_charge.get::<String, _>("match_status"), "Pending");
    assert_eq!(reset_charge.get::<Option<String>, _>("match_source"), None);
    assert!(reset_charge.get::<i64, _>("match_started_at") > 1700000100);
}

#[tokio::test]
async fn migration_0012_keeps_legacy_subscription_writers_working() {
    let postgres = LegacyPostgres::at_0011().await;
    let fixture = seed_representative_legacy_rows(&postgres.pool).await;

    migrate_through(&postgres.pool, 12).await;

    // Simulate a replica that is still running the 0011-era INSERT and UPDATE
    // statements after the expand migration has committed.
    let account_id: Uuid = sqlx::query_scalar("SELECT account_id FROM transactions WHERE id=$1")
        .bind(fixture.transaction_id)
        .fetch_one(&postgres.pool)
        .await
        .unwrap();
    let transaction_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO transactions
            (id, account_id, user_id, amount, currency, kind, transacted_at, created_at)
         VALUES ($1, $2, $3, 19.99, 'USD', 'Expense', now(), now())",
    )
    .bind(transaction_id)
    .bind(account_id)
    .bind(fixture.user_id)
    .execute(&postgres.pool)
    .await
    .unwrap();

    let subscription_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO subscriptions
            (id, user_id, provider, product_name, merchant_key, amount, currency,
             billing_period, status, started_at, last_charged_at, next_expected_at,
             category_id, created_at)
         VALUES ($1, $2, 'legacy-provider', 'Rolling Writer', 'rolling-writer',
                 19.99, 'USD', 'monthly', 'active', 1701000000, 1701000100,
                 1703678500, NULL, 1701000200)",
    )
    .bind(subscription_id)
    .bind(fixture.user_id)
    .execute(&postgres.pool)
    .await
    .unwrap();

    // An old writer updating last_charged_at must advance, not regress, the
    // new chronology column that it does not know about.
    sqlx::query("UPDATE subscriptions SET last_charged_at=1701000400 WHERE id=$1")
        .bind(subscription_id)
        .execute(&postgres.pool)
        .await
        .unwrap();

    let charge_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO subscription_charges
            (id, subscription_id, user_id, amount, currency, charged_at,
             email_message_id, kind, transaction_id, match_status, created_at)
         VALUES ($1, $2, $3, 19.99, 'USD', 1701000100,
                 '<rolling-writer@example.com>', 'renewal', NULL, 'Pending',
                 1701000200)",
    )
    .bind(charge_id)
    .bind(subscription_id)
    .bind(fixture.user_id)
    .execute(&postgres.pool)
    .await
    .unwrap();
    sqlx::query(
        "UPDATE subscription_charges
         SET transaction_id=$1, match_status='Matched'
         WHERE id=$2",
    )
    .bind(transaction_id)
    .bind(charge_id)
    .execute(&postgres.pool)
    .await
    .unwrap();

    let normalized = sqlx::query(
        "SELECT email_message_id, source_key, rfc_message_id, match_started_at,
                match_source
         FROM subscription_charges WHERE id=$1",
    )
    .bind(charge_id)
    .fetch_one(&postgres.pool)
    .await
    .unwrap();
    let expected_key = format!("legacy:{}:<rolling-writer@example.com>", fixture.user_id);
    assert_eq!(
        normalized.get::<String, _>("email_message_id"),
        expected_key
    );
    assert_eq!(normalized.get::<String, _>("source_key"), expected_key);
    assert_eq!(
        normalized
            .get::<Option<String>, _>("rfc_message_id")
            .as_deref(),
        Some("<rolling-writer@example.com>")
    );
    assert_eq!(normalized.get::<i64, _>("match_started_at"), 1701000200);
    assert_eq!(
        normalized
            .get::<Option<String>, _>("match_source")
            .as_deref(),
        Some("automatic")
    );
    let last_receipt_at: i64 =
        sqlx::query_scalar("SELECT last_receipt_at FROM subscriptions WHERE id=$1")
            .bind(subscription_id)
            .fetch_one(&postgres.pool)
            .await
            .unwrap();
    assert_eq!(last_receipt_at, 1701000400);

    let duplicate_link = sqlx::query(
        "INSERT INTO subscription_charges
            (id, subscription_id, user_id, amount, currency, charged_at,
             email_message_id, kind, transaction_id, match_status, created_at)
         VALUES ($1, $2, $3, 19.99, 'USD', 1701000300,
                 '<rolling-duplicate@example.com>', 'renewal', $4, 'Matched',
                 1701000300)",
    )
    .bind(Uuid::new_v4())
    .bind(subscription_id)
    .bind(fixture.user_id)
    .bind(transaction_id)
    .execute(&postgres.pool)
    .await
    .unwrap_err();
    assert_eq!(
        duplicate_link.as_database_error().unwrap().constraint(),
        Some("subscription_charges_transaction_unique")
    );

    MIGRATOR.run(&postgres.pool).await.unwrap();
    let mapped_charge = PgSubscriptionChargeRepository::new(postgres.pool.clone())
        .find_by_id(charge_id, fixture.user_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(mapped_charge.transaction_id, Some(transaction_id));
    assert_eq!(
        mapped_charge.match_source.map(|source| source.as_str()),
        Some("automatic")
    );
}

#[tokio::test]
async fn migration_0013_keeps_legacy_email_connection_writers_working() {
    let postgres = LegacyPostgres::at_0011().await;
    migrate_through(&postgres.pool, 13).await;

    let user_id = Uuid::new_v4();
    let connection_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO email_connections
            (id, user_id, provider, email_address, oauth_access_token,
             oauth_refresh_token, access_token_expires_at, status, created_at)
         VALUES ($1, $2, 'gmail', '  Rolling.User@Example.COM  ',
                 'legacy-access', 'legacy-refresh', 1700003600, 'connected',
                 1700000000)",
    )
    .bind(connection_id)
    .bind(user_id)
    .execute(&postgres.pool)
    .await
    .unwrap();

    let normalized_address: String =
        sqlx::query_scalar("SELECT email_address FROM email_connections WHERE id=$1")
            .bind(connection_id)
            .fetch_one(&postgres.pool)
            .await
            .unwrap();
    assert_eq!(normalized_address, "rolling.user@example.com");

    // Old replicas can update credentials or the address without knowing
    // about the normalized-address constraint.
    sqlx::query(
        "UPDATE email_connections
         SET oauth_access_token='rotated-access',
             email_address=' ROLLING.RENAMED@EXAMPLE.COM '
         WHERE id=$1",
    )
    .bind(connection_id)
    .execute(&postgres.pool)
    .await
    .unwrap();
    let renamed_address: String =
        sqlx::query_scalar("SELECT email_address FROM email_connections WHERE id=$1")
            .bind(connection_id)
            .fetch_one(&postgres.pool)
            .await
            .unwrap();
    assert_eq!(renamed_address, "rolling.renamed@example.com");

    let duplicate = sqlx::query(
        "INSERT INTO email_connections
            (id, user_id, provider, email_address, oauth_access_token,
             oauth_refresh_token, access_token_expires_at, status, created_at)
         VALUES ($1, $2, 'gmail', 'rolling.renamed@EXAMPLE.com',
                 'other-access', 'other-refresh', 1700003600, 'connected',
                 1700000000)",
    )
    .bind(Uuid::new_v4())
    .bind(user_id)
    .execute(&postgres.pool)
    .await
    .unwrap_err();
    let duplicate_error = duplicate.as_database_error().unwrap();
    assert_eq!(
        duplicate_error.constraint(),
        Some("email_connections_user_provider_address_unique")
    );
    assert_eq!(
        duplicate_error.message(),
        format!(
            "email connection already exists for user={user_id} provider=gmail normalized_address=rolling.renamed@example.com"
        )
    );

    // Multiple distinct Gmail mailboxes for one user remain supported.
    sqlx::query(
        "INSERT INTO email_connections
            (id, user_id, provider, email_address, oauth_access_token,
             oauth_refresh_token, access_token_expires_at, status, created_at)
         VALUES ($1, $2, 'gmail', 'second@example.com', 'second-access',
                 'second-refresh', 1700003600, 'connected', 1700000000)",
    )
    .bind(Uuid::new_v4())
    .bind(user_id)
    .execute(&postgres.pool)
    .await
    .unwrap();

    MIGRATOR.run(&postgres.pool).await.unwrap();
    let identity_index_valid: bool = sqlx::query_scalar(
        "SELECT indisvalid
         FROM pg_index
         WHERE indexrelid='email_connections_user_provider_address_unique'::regclass",
    )
    .fetch_one(&postgres.pool)
    .await
    .unwrap();
    assert!(identity_index_valid);
}

#[tokio::test]
async fn migration_0016_refuses_duplicate_transaction_links_with_remediation() {
    let postgres = LegacyPostgres::at_0011().await;
    let fixture = seed_representative_legacy_rows(&postgres.pool).await;

    sqlx::query(
        "INSERT INTO subscription_charges
            (id, subscription_id, user_id, amount, currency, charged_at,
             email_message_id, kind, transaction_id, match_status, created_at)
         VALUES ($1, $2, $3, 129.99, 'UAH', 1700000200,
                 '<duplicate-link@example.com>', 'renewal', $4, 'Matched',
                 1700000300)",
    )
    .bind(Uuid::new_v4())
    .bind(fixture.subscription_id)
    .bind(fixture.user_id)
    .bind(fixture.transaction_id)
    .execute(&postgres.pool)
    .await
    .unwrap();

    let error = MIGRATOR
        .run(&postgres.pool)
        .await
        .expect_err("migration must not guess which charge owns a transaction");
    let sqlx::migrate::MigrateError::ExecuteMigration(error, 16) = error else {
        panic!("expected migration 16 execution failure, got {error:?}");
    };
    let database_error = error
        .as_database_error()
        .expect("expected PostgreSQL migration error");
    let postgres_error = database_error
        .try_downcast_ref::<PgDatabaseError>()
        .expect("expected PostgreSQL-specific error");
    assert!(
        postgres_error
            .message()
            .contains("cannot enforce one charge per transaction")
    );
    assert!(
        postgres_error
            .message()
            .contains("duplicate transaction link")
    );
    assert_eq!(
        postgres_error.hint(),
        Some(
            "Unlink all but the intended subscription charge for each duplicated transaction, then retry migration 0016."
        )
    );

    // The expand migration is intentionally committed before the backfill.
    // A failed preflight leaves the compatibility trigger and additive columns
    // available, so old replicas keep working while the operator repairs data.
    let source_column: Option<String> = sqlx::query_scalar(
        "SELECT column_name
         FROM information_schema.columns
         WHERE table_schema = 'public'
           AND table_name = 'subscription_charges'
           AND column_name = 'source'",
    )
    .fetch_optional(&postgres.pool)
    .await
    .unwrap()
    .flatten();
    assert_eq!(source_column.as_deref(), Some("source"));

    // Versions through the credential-fencing expand are safely recorded
    // before the data preflight fails.
    let applied_versions: Vec<i64> =
        sqlx::query_scalar("SELECT version FROM _sqlx_migrations ORDER BY version")
            .fetch_all(&postgres.pool)
            .await
            .unwrap();
    assert_eq!(applied_versions, (1_i64..=15).collect::<Vec<_>>());
}

#[tokio::test]
async fn migration_0023_refuses_normalized_mailbox_collisions_with_remediation() {
    let postgres = LegacyPostgres::at_0011().await;
    let user_id = Uuid::new_v4();
    for (id, address) in [
        (Uuid::new_v4(), " Collision@Example.COM "),
        (Uuid::new_v4(), "collision@example.com"),
    ] {
        sqlx::query(
            "INSERT INTO email_connections \
             (id,user_id,provider,email_address,oauth_access_token,oauth_refresh_token,\
              access_token_expires_at,status,created_at) \
             VALUES ($1,$2,'gmail',$3,'access','refresh',1700000000,'connected',1700000000)",
        )
        .bind(id)
        .bind(user_id)
        .bind(address)
        .execute(&postgres.pool)
        .await
        .unwrap();
    }

    let error = MIGRATOR
        .run(&postgres.pool)
        .await
        .expect_err("migration must reject normalized mailbox collisions");
    let sqlx::migrate::MigrateError::ExecuteMigration(error, 23) = error else {
        panic!("expected migration 23 execution failure, got {error:?}");
    };
    let database_error = error.as_database_error().expect("PostgreSQL error");
    let postgres_error = database_error
        .try_downcast_ref::<PgDatabaseError>()
        .expect("PostgreSQL-specific error");
    assert!(
        postgres_error
            .message()
            .contains("cannot normalize email connections")
    );
    assert!(postgres_error.message().contains("collision@example.com"));
    assert_eq!(
        postgres_error.hint(),
        Some(
            "Merge or remove duplicate rows grouped by user_id, provider, and lower(btrim(email_address)), then retry migration 0023."
        )
    );

    let encrypted_column: Option<String> = sqlx::query_scalar(
        "SELECT column_name FROM information_schema.columns \
         WHERE table_name='email_connections' AND column_name='oauth_access_token_encrypted'",
    )
    .fetch_optional(&postgres.pool)
    .await
    .unwrap()
    .flatten();
    assert_eq!(
        encrypted_column.as_deref(),
        Some("oauth_access_token_encrypted")
    );

    let applied_versions: Vec<i64> =
        sqlx::query_scalar("SELECT version FROM _sqlx_migrations ORDER BY version")
            .fetch_all(&postgres.pool)
            .await
            .unwrap();
    assert_eq!(applied_versions, (1_i64..=22).collect::<Vec<_>>());
}

#[tokio::test]
async fn fresh_postgres_16_migrates_through_latest() {
    let postgres = LegacyPostgres::empty().await;
    MIGRATOR.run(&postgres.pool).await.unwrap();

    let versions: Vec<i64> =
        sqlx::query_scalar("SELECT version FROM _sqlx_migrations ORDER BY version")
            .fetch_all(&postgres.pool)
            .await
            .unwrap();
    assert_eq!(versions, (1_i64..=LATEST_MIGRATION).collect::<Vec<_>>());
    let charge_table: Option<String> =
        sqlx::query_scalar("SELECT to_regclass('subscription_charges')::text")
            .fetch_one(&postgres.pool)
            .await
            .unwrap();
    assert_eq!(charge_table.as_deref(), Some("subscription_charges"));
}
