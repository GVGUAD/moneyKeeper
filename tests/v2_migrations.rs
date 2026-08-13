use moneykeeper::infrastructure::v2_db::initialize_v2;
use moneykeeper::infrastructure::v2_test_db::{FreshV2Database, create_fresh_database};
use sqlx::{Executor, PgPool};
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, ImageExt};
use testcontainers_modules::postgres::Postgres;
use tokio::sync::OnceCell;

static CONTAINER: OnceCell<SharedPostgres> = OnceCell::const_new();

struct SharedPostgres {
    _container: ContainerAsync<Postgres>,
    admin_url: String,
}

async fn postgres() -> &'static SharedPostgres {
    CONTAINER
        .get_or_init(|| async {
            let container = Postgres::default()
                .with_tag("16-alpine")
                .start()
                .await
                .expect("start PostgreSQL 16 testcontainer");
            let port = container
                .get_host_port_ipv4(5432)
                .await
                .expect("resolve PostgreSQL test port");
            SharedPostgres {
                _container: container,
                admin_url: format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres"),
            }
        })
        .await
}

async fn fresh_database() -> FreshV2Database {
    create_fresh_database(&postgres().await.admin_url)
        .await
        .expect("create an isolated database")
}

async fn assert_rejected_before_v2_migrations(database: &FreshV2Database) {
    let error = initialize_v2(database.database_url())
        .await
        .expect_err("unmarked non-empty database must be rejected");
    assert!(
        format!("{error:#}").contains("refusing non-Finance-V2 database"),
        "unexpected error: {error:#}"
    );

    let pool = PgPool::connect(database.database_url()).await.unwrap();
    let migration_history: Option<String> =
        sqlx::query_scalar("SELECT to_regclass('_sqlx_migrations')::text")
            .fetch_one(&pool)
            .await
            .unwrap();
    let v2_marker: Option<String> =
        sqlx::query_scalar("SELECT to_regclass('shared_kernel.database_lineage')::text")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(migration_history.is_none());
    assert!(v2_marker.is_none());
}

#[tokio::test]
async fn empty_database_passes_v2_preflight() {
    let database = fresh_database().await;
    database.initialize().await.expect("initialize empty V2 DB");
}

#[tokio::test]
async fn marked_v2_database_passes_v2_preflight() {
    let database = fresh_database().await;
    database.initialize().await.expect("initialize V2 DB");
    initialize_v2(database.database_url())
        .await
        .expect("reopen marked V2 DB");
}

#[tokio::test]
async fn legacy_sqlx_database_is_rejected_before_v2_migrations_run() {
    let database = fresh_database().await;
    let pool = PgPool::connect(database.database_url())
        .await
        .expect("connect to empty database");
    sqlx::migrate!("src/infrastructure/migrations")
        .run(&pool)
        .await
        .expect("run the real legacy migration lineage");
    let legacy_count: i64 = sqlx::query_scalar("SELECT count(*) FROM _sqlx_migrations")
        .fetch_one(&pool)
        .await
        .unwrap();
    pool.close().await;

    let error = initialize_v2(database.database_url())
        .await
        .expect_err("legacy database must be rejected");
    assert!(
        format!("{error:#}").contains("refusing non-Finance-V2 database"),
        "unexpected error: {error:#}"
    );

    let pool = PgPool::connect(database.database_url()).await.unwrap();
    let count_after: i64 = sqlx::query_scalar("SELECT count(*) FROM _sqlx_migrations")
        .fetch_one(&pool)
        .await
        .unwrap();
    let v2_marker: Option<String> =
        sqlx::query_scalar("SELECT to_regclass('shared_kernel.database_lineage')::text")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(count_after, legacy_count);
    assert!(v2_marker.is_none());
}

#[tokio::test]
async fn nonempty_unmarked_database_is_rejected_before_v2_migrations_run() {
    let database = fresh_database().await;
    let pool = PgPool::connect(database.database_url()).await.unwrap();
    pool.execute("CREATE TABLE public.unrelated_data (id BIGINT PRIMARY KEY)")
        .await
        .unwrap();
    pool.close().await;

    let error = initialize_v2(database.database_url())
        .await
        .expect_err("arbitrary non-empty database must be rejected");
    assert!(
        format!("{error:#}").contains("refusing non-Finance-V2 database"),
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
async fn empty_custom_schema_is_rejected_before_v2_migrations_run() {
    let database = fresh_database().await;
    let pool = PgPool::connect(database.database_url()).await.unwrap();
    pool.execute("CREATE SCHEMA arbitrary_application")
        .await
        .unwrap();
    pool.close().await;

    assert_rejected_before_v2_migrations(&database).await;
}

#[tokio::test]
async fn public_types_routines_and_procedures_are_rejected_before_v2_migrations_run() {
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

        assert_rejected_before_v2_migrations(&database).await;
    }
}

#[tokio::test]
async fn nondefault_extension_is_rejected_before_v2_migrations_run() {
    let database = fresh_database().await;
    let pool = PgPool::connect(database.database_url()).await.unwrap();
    pool.execute("CREATE EXTENSION hstore")
        .await
        .expect("PostgreSQL 16 test image must provide hstore");
    pool.close().await;

    assert_rejected_before_v2_migrations(&database).await;
}

#[tokio::test]
async fn empty_database_is_initialized_as_finance_v2() {
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
async fn already_marked_v2_database_is_reopened_idempotently() {
    let database = fresh_database().await;
    database.initialize().await.unwrap();
    let verified = initialize_v2(database.database_url()).await.unwrap();
    let mut connection = verified.acquire().await.unwrap();

    let marker_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM shared_kernel.database_lineage")
            .fetch_one(&mut *connection)
            .await
            .unwrap();
    assert_eq!(marker_count, 1);
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

    for (code, minor_unit) in [("usd", 2_i16), ("US", 2), ("USDX", 2), ("GBP", 9)] {
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
         (id, user_id, name, kind) VALUES ($1, $2, 'Food', 'expense')",
    )
    .bind(category_id)
    .bind(user_id)
    .execute(&mut *connection)
    .await
    .unwrap();
    let duplicate = sqlx::query(
        "INSERT INTO classification.categories \
         (id, user_id, name, kind) VALUES ($1, $2, 'fOoD', 'expense')",
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
             (id, user_id, name, kind, lifecycle, version) \
             VALUES ($1, $2, $3, $4, $5, $6)",
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
