//! Test support for creating isolated PostgreSQL databases for Moneykeeper.
//!
//! Container lifecycle remains in integration tests, so Testcontainers does not
//! become a production dependency. This module only creates a uniquely named
//! database through an already-running PostgreSQL admin endpoint.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

use anyhow::{Context, ensure};
use sqlx::postgres::PgPoolOptions;
use sqlx::{Connection, Executor, PgConnection};
use uuid::Uuid;

use super::database::{VerifiedDatabase, initialize_with_pool_limit_and_guards};

const TEST_POOL_MAX_CONNECTIONS: u32 = 3;
static DATABASE_INITIALIZATION_PERMITS: LazyLock<Arc<tokio::sync::Semaphore>> =
    LazyLock::new(|| Arc::new(tokio::sync::Semaphore::new(1)));

static DATABASE_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A uniquely named empty PostgreSQL database owned by a test container.
pub struct FreshDatabase {
    database_url: String,
    initialization_permit: Arc<Mutex<Option<tokio::sync::OwnedSemaphorePermit>>>,
    lifetime_guards: Vec<Arc<dyn Send + Sync>>,
}

impl FreshDatabase {
    /// Returns the connection URL without logging or otherwise exposing it.
    pub fn database_url(&self) -> &str {
        &self.database_url
    }

    /// Runs the guarded Moneykeeper initialization path.
    pub async fn initialize(&self) -> anyhow::Result<VerifiedDatabase> {
        let result = initialize_with_pool_limit_and_guards(
            &self.database_url,
            TEST_POOL_MAX_CONNECTIONS,
            self.lifetime_guards.clone(),
        )
        .await;
        self.initialization_permit
            .lock()
            .expect("Moneykeeper test initialization permit mutex poisoned")
            .take();
        result
    }

    /// Opens a bounded raw pool for integration assertions.
    pub async fn connect(&self) -> anyhow::Result<sqlx::PgPool> {
        PgPoolOptions::new()
            .max_connections(TEST_POOL_MAX_CONNECTIONS)
            .connect(&self.database_url)
            .await
            .context("connect to isolated Moneykeeper test database")
    }

    #[doc(hidden)]
    pub fn with_lifetime_guard(mut self, guard: Arc<dyn Send + Sync>) -> Self {
        self.lifetime_guards.push(guard);
        self
    }
}

/// Creates a unique empty database through `admin_database_url`.
///
/// # Errors
///
/// Returns an error when the admin URL is malformed or PostgreSQL cannot create
/// or connect to the database.
pub async fn create_fresh_database(admin_database_url: &str) -> anyhow::Result<FreshDatabase> {
    let initialization_permit = Arc::clone(&DATABASE_INITIALIZATION_PERMITS)
        .acquire_owned()
        .await
        .context("acquire Moneykeeper test database concurrency permit")?;
    let sequence = DATABASE_COUNTER.fetch_add(1, Ordering::SeqCst);
    let database_name = format!("moneykeeper_test_{}_{}", sequence, Uuid::new_v4().simple());

    let mut admin = PgConnection::connect(admin_database_url)
        .await
        .context("connect to PostgreSQL test administrator database")?;
    admin
        .execute(format!(r#"CREATE DATABASE "{database_name}""#).as_str())
        .await
        .context("create isolated Moneykeeper test database")?;
    admin.close().await.ok();

    Ok(FreshDatabase {
        database_url: replace_database_name(admin_database_url, &database_name)?,
        initialization_permit: Arc::new(Mutex::new(Some(initialization_permit))),
        lifetime_guards: Vec::new(),
    })
}

fn replace_database_name(admin_database_url: &str, database_name: &str) -> anyhow::Result<String> {
    let (base, query) = admin_database_url
        .split_once('?')
        .map_or((admin_database_url, None), |(base, query)| {
            (base, Some(query))
        });
    let slash = base
        .rfind('/')
        .context("PostgreSQL admin URL must include a database path")?;
    ensure!(
        slash > "postgres://".len(),
        "PostgreSQL admin URL must include a host and database path"
    );

    let mut result = format!("{}/{database_name}", &base[..slash]);
    if let Some(query) = query {
        result.push('?');
        result.push_str(query);
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::replace_database_name;

    #[test]
    fn replaces_database_and_preserves_query() {
        assert_eq!(
            replace_database_name(
                "postgres://user:password@localhost:5432/postgres?sslmode=disable",
                "moneykeeper_test"
            )
            .unwrap(),
            "postgres://user:password@localhost:5432/moneykeeper_test?sslmode=disable"
        );
    }
}
