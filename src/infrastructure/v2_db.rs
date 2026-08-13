//! Guarded database initialization for the parallel Finance V2 lineage.

use std::fmt;

use anyhow::{Context, bail, ensure};
use sqlx::migrate::Migrator;
use sqlx::pool::PoolConnection;
use sqlx::postgres::{PgPool, PgPoolOptions};
use sqlx::{Postgres, Transaction};

/// The immutable Finance V2 migration lineage, including the parallel Ledger baseline.
pub static V2_MIGRATOR: Migrator = sqlx::migrate!("src/infrastructure/migrations_v2");

const DATABASE_LINEAGE: &str = "finance-v2";

/// A PostgreSQL pool that has passed the Finance V2 lineage guard.
///
/// There is deliberately no unchecked public constructor. Call [`initialize_v2`]
/// before building any V2 context, router, or worker.
#[derive(Clone)]
pub struct VerifiedV2Pool {
    pool: PgPool,
}

impl fmt::Debug for VerifiedV2Pool {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VerifiedV2Pool")
            .finish_non_exhaustive()
    }
}

impl VerifiedV2Pool {
    /// Acquires one connection from the verified pool.
    pub async fn acquire(&self) -> Result<PoolConnection<Postgres>, sqlx::Error> {
        self.pool.acquire().await
    }

    /// Begins a transaction on the verified pool.
    pub async fn begin(&self) -> Result<Transaction<'_, Postgres>, sqlx::Error> {
        self.pool.begin().await
    }

    /// Returns the raw handle only to in-crate V2 composition and adapters.
    ///
    /// External callers intentionally receive only bounded connection and
    /// transaction access, so they cannot pass a cloned raw pool to an
    /// unchecked V2 constructor.
    pub(crate) fn pool(&self) -> &PgPool {
        &self.pool
    }
}

/// Connects to, migrates, and verifies a Finance V2 database.
///
/// # Errors
///
/// Returns an error when the database cannot be reached, contains an unmarked
/// legacy or arbitrary schema, fails a migration, or does not match the complete
/// embedded Finance V2 lineage after migration.
pub async fn initialize_v2(database_url: &str) -> anyhow::Result<VerifiedV2Pool> {
    let pool = create_v2_pool(database_url).await?;
    migrate_v2(&pool).await?;
    Ok(VerifiedV2Pool { pool })
}

pub(crate) async fn create_v2_pool(database_url: &str) -> anyhow::Result<PgPool> {
    PgPoolOptions::new()
        .max_connections(10)
        .connect(database_url)
        .await
        .context("connect to Finance V2 PostgreSQL database")
}

pub(crate) async fn migrate_v2(pool: &PgPool) -> anyhow::Result<()> {
    preflight(pool).await?;
    V2_MIGRATOR
        .run(pool)
        .await
        .context("run Finance V2 migrations")?;
    verify_marker(pool).await?;
    verify_complete_lineage(pool).await
}

async fn preflight(pool: &PgPool) -> anyhow::Result<()> {
    let marker_table: Option<String> =
        sqlx::query_scalar("SELECT to_regclass('shared_kernel.database_lineage')::text")
            .fetch_one(pool)
            .await
            .context("inspect Finance V2 lineage marker")?;

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
        bail!("refusing non-Finance-V2 database: the database is non-empty and unmarked");
    }

    Ok(())
}

async fn verify_marker(pool: &PgPool) -> anyhow::Result<()> {
    let marker_table: Option<String> =
        sqlx::query_scalar("SELECT to_regclass('shared_kernel.database_lineage')::text")
            .fetch_one(pool)
            .await
            .context("inspect Finance V2 lineage marker")?;

    if marker_table.is_none() {
        bail!("refusing non-Finance-V2 database: Finance V2 lineage marker is absent");
    }

    let rows: Vec<(bool, String)> =
        sqlx::query_as("SELECT singleton, lineage FROM shared_kernel.database_lineage")
            .fetch_all(pool)
            .await
            .map_err(|error| {
                anyhow::anyhow!("refusing non-Finance-V2 database: invalid lineage marker: {error}")
            })?;

    ensure!(
        rows.as_slice() == [(true, DATABASE_LINEAGE.to_owned())],
        "refusing non-Finance-V2 database: invalid Finance V2 lineage marker"
    );
    Ok(())
}

async fn verify_complete_lineage(pool: &PgPool) -> anyhow::Result<()> {
    let applied: Vec<i64> =
        sqlx::query_scalar("SELECT version FROM _sqlx_migrations WHERE success ORDER BY version")
            .fetch_all(pool)
            .await
            .context("read applied Finance V2 migration lineage")?;
    let expected: Vec<i64> = V2_MIGRATOR
        .iter()
        .filter(|migration| migration.migration_type.is_up_migration())
        .map(|migration| migration.version)
        .collect();

    ensure!(
        applied == expected,
        "Finance V2 database migration lineage is incomplete: expected {expected:?}, found {applied:?}"
    );
    Ok(())
}
