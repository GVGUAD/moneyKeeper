//! Guarded initialization for the active Moneykeeper database lineage.

use std::{fmt, sync::Arc};

use anyhow::{Context, bail, ensure};
use sqlx::migrate::Migrator;
use sqlx::pool::PoolConnection;
use sqlx::postgres::{PgPool, PgPoolOptions};
use sqlx::{Postgres, Transaction};

/// The immutable, complete Moneykeeper migration lineage embedded in this binary.
pub static DATABASE_MIGRATOR: Migrator = sqlx::migrate!("src/infrastructure/migrations_v2");

const DATABASE_LINEAGE: &str = "finance-v2";

/// A PostgreSQL pool that has passed the Moneykeeper lineage guard.
///
/// There is deliberately no unchecked public constructor. Call [`initialize_database`]
/// before building any context, router, or worker.
#[derive(Clone)]
pub struct VerifiedDatabase {
    pool: PgPool,
    _lifetime_guards: Vec<Arc<dyn Send + Sync>>,
}

impl fmt::Debug for VerifiedDatabase {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VerifiedDatabase")
            .finish_non_exhaustive()
    }
}

impl VerifiedDatabase {
    /// Acquires one connection from the verified pool.
    pub async fn acquire(&self) -> Result<PoolConnection<Postgres>, sqlx::Error> {
        self.pool.acquire().await
    }

    /// Begins a transaction on the verified pool.
    pub async fn begin(&self) -> Result<Transaction<'_, Postgres>, sqlx::Error> {
        self.pool.begin().await
    }

    /// Returns the raw handle only to in-crate composition and adapters.
    ///
    /// External callers intentionally receive only bounded connection and
    /// transaction access, so they cannot pass a cloned raw pool to an
    /// unchecked constructor.
    pub(crate) fn pool(&self) -> &PgPool {
        &self.pool
    }
}

/// Connects to, migrates, and verifies a Moneykeeper database.
///
/// # Errors
///
/// Returns an error when the database cannot be reached, contains an unmarked
/// legacy or arbitrary schema, fails a migration, or does not match the complete
/// embedded Moneykeeper lineage after migration.
pub async fn initialize_database(database_url: &str) -> anyhow::Result<VerifiedDatabase> {
    initialize_with_pool_limit_and_guards(database_url, 10, Vec::new()).await
}

pub(crate) async fn initialize_with_pool_limit_and_guards(
    database_url: &str,
    maximum_connections: u32,
    lifetime_guards: Vec<Arc<dyn Send + Sync>>,
) -> anyhow::Result<VerifiedDatabase> {
    ensure!(
        maximum_connections > 0,
        "database pool limit must be positive"
    );
    let pool = connect_pool(database_url, maximum_connections, lifetime_guards.clone()).await?;
    migrate_database(&pool).await?;
    Ok(VerifiedDatabase {
        pool,
        _lifetime_guards: lifetime_guards,
    })
}

async fn connect_pool(
    database_url: &str,
    maximum_connections: u32,
    lifetime_guards: Vec<Arc<dyn Send + Sync>>,
) -> anyhow::Result<PgPool> {
    let options = PgPoolOptions::new().max_connections(maximum_connections);
    let options = if lifetime_guards.is_empty() {
        options
    } else {
        let lifetime_guards = Arc::new(lifetime_guards);
        options.after_connect(move |_connection, _metadata| {
            let lifetime_guards = Arc::clone(&lifetime_guards);
            Box::pin(async move {
                // SQLx stores this callback in the pool, so every raw pool clone
                // retained by a context also retains the testcontainer guards.
                drop(lifetime_guards);
                Ok(())
            })
        })
    };
    options
        .connect(database_url)
        .await
        .context("connect to Moneykeeper PostgreSQL database")
}

async fn migrate_database(pool: &PgPool) -> anyhow::Result<()> {
    preflight(pool).await?;
    DATABASE_MIGRATOR
        .run(pool)
        .await
        .context("run Moneykeeper migrations")?;
    verify_marker(pool).await?;
    verify_complete_lineage(pool).await
}

async fn preflight(pool: &PgPool) -> anyhow::Result<()> {
    let marker_table: Option<String> =
        sqlx::query_scalar("SELECT to_regclass('shared_kernel.database_lineage')::text")
            .fetch_one(pool)
            .await
            .context("inspect Moneykeeper lineage marker")?;

    if marker_table.is_some() {
        return verify_marker(pool).await;
    }

    let has_sqlx_history: bool =
        sqlx::query_scalar("SELECT to_regclass('_sqlx_migrations') IS NOT NULL")
            .fetch_one(pool)
            .await
            .context("inspect SQLx migration history")?;

    let has_non_system_objects: bool = sqlx::query_scalar(
        r#"
        SELECT
            EXISTS (
                SELECT 1
                FROM pg_catalog.pg_namespace AS nsp
                WHERE nsp.nspname NOT IN (
                    'public', 'pg_catalog', 'information_schema'
                )
                  AND nsp.nspname NOT LIKE 'pg_toast%'
                  AND nsp.nspname NOT LIKE 'pg_temp_%'
            )
            OR EXISTS (
                SELECT 1
                FROM (
                    SELECT cls.relnamespace AS namespace_oid
                    FROM pg_catalog.pg_class AS cls

                    UNION ALL

                    SELECT proc_obj.pronamespace
                    FROM pg_catalog.pg_proc AS proc_obj

                    UNION ALL

                    SELECT typ.typnamespace
                    FROM pg_catalog.pg_type AS typ

                    UNION ALL

                    SELECT coll.collnamespace
                    FROM pg_catalog.pg_collation AS coll

                    UNION ALL

                    SELECT conv.connamespace
                    FROM pg_catalog.pg_conversion AS conv

                    UNION ALL

                    SELECT opr.oprnamespace
                    FROM pg_catalog.pg_operator AS opr

                    UNION ALL

                    SELECT opc.opcnamespace
                    FROM pg_catalog.pg_opclass AS opc

                    UNION ALL

                    SELECT opf.opfnamespace
                    FROM pg_catalog.pg_opfamily AS opf

                    UNION ALL

                    SELECT stx.stxnamespace
                    FROM pg_catalog.pg_statistic_ext AS stx

                    UNION ALL

                    SELECT cfg.cfgnamespace
                    FROM pg_catalog.pg_ts_config AS cfg

                    UNION ALL

                    SELECT dict_obj.dictnamespace
                    FROM pg_catalog.pg_ts_dict AS dict_obj

                    UNION ALL

                    SELECT prs.prsnamespace
                    FROM pg_catalog.pg_ts_parser AS prs

                    UNION ALL

                    SELECT tmpl.tmplnamespace
                    FROM pg_catalog.pg_ts_template AS tmpl
                ) AS obj
                JOIN pg_catalog.pg_namespace AS nsp
                  ON nsp.oid = obj.namespace_oid
                WHERE nsp.nspname = 'public'
            )
            OR EXISTS (
                SELECT 1
                FROM pg_catalog.pg_extension AS ext
                JOIN pg_catalog.pg_namespace AS nsp
                  ON nsp.oid = ext.extnamespace
                WHERE NOT (
                    ext.extname = 'plpgsql'
                    AND nsp.nspname = 'pg_catalog'
                )
            )
        "#,
    )
    .fetch_one(pool)
    .await
    .context("inspect existing non-system database objects")?;

    if has_sqlx_history || has_non_system_objects {
        bail!("refusing non-Moneykeeper database: the database is non-empty and unmarked");
    }

    Ok(())
}

async fn verify_marker(pool: &PgPool) -> anyhow::Result<()> {
    let marker_table: Option<String> =
        sqlx::query_scalar("SELECT to_regclass('shared_kernel.database_lineage')::text")
            .fetch_one(pool)
            .await
            .context("inspect Moneykeeper lineage marker")?;

    if marker_table.is_none() {
        bail!("refusing non-Moneykeeper database: Moneykeeper lineage marker is absent");
    }

    let rows: Vec<(bool, String)> =
        sqlx::query_as("SELECT singleton, lineage FROM shared_kernel.database_lineage")
            .fetch_all(pool)
            .await
            .map_err(|error| {
                anyhow::anyhow!(
                    "refusing non-Moneykeeper database: invalid lineage marker: {error}"
                )
            })?;

    ensure!(
        rows.as_slice() == [(true, DATABASE_LINEAGE.to_owned())],
        "refusing non-Moneykeeper database: invalid Moneykeeper lineage marker"
    );
    Ok(())
}

async fn verify_complete_lineage(pool: &PgPool) -> anyhow::Result<()> {
    let applied: Vec<i64> =
        sqlx::query_scalar("SELECT version FROM _sqlx_migrations WHERE success ORDER BY version")
            .fetch_all(pool)
            .await
            .context("read applied Moneykeeper migration lineage")?;
    let expected: Vec<i64> = DATABASE_MIGRATOR
        .iter()
        .filter(|migration| migration.migration_type.is_up_migration())
        .map(|migration| migration.version)
        .collect();

    ensure!(
        !expected.is_empty(),
        "Moneykeeper binary contains no embedded migration baseline"
    );

    ensure!(
        applied == expected,
        "Moneykeeper database migration lineage is incomplete: expected {expected:?}, found {applied:?}"
    );
    Ok(())
}
