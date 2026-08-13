//! Test support for creating isolated PostgreSQL databases for Finance V2.
//!
//! Container lifecycle remains in integration tests, so Testcontainers does not
//! become a production dependency. This module only creates a uniquely named
//! database through an already-running PostgreSQL admin endpoint.

use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, ensure};
use sqlx::{Connection, Executor, PgConnection};
use uuid::Uuid;

use super::v2_db::{VerifiedV2Pool, initialize_v2};

static DATABASE_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A uniquely named empty PostgreSQL database owned by a test container.
#[derive(Clone)]
pub struct FreshV2Database {
    database_url: String,
}

impl FreshV2Database {
    /// Returns the connection URL without logging or otherwise exposing it.
    pub fn database_url(&self) -> &str {
        &self.database_url
    }

    /// Runs the guarded Finance V2 initialization path.
    pub async fn initialize(&self) -> anyhow::Result<VerifiedV2Pool> {
        initialize_v2(&self.database_url).await
    }
}

/// Creates a unique empty database through `admin_database_url`.
///
/// # Errors
///
/// Returns an error when the admin URL is malformed or PostgreSQL cannot create
/// or connect to the database.
pub async fn create_fresh_database(admin_database_url: &str) -> anyhow::Result<FreshV2Database> {
    let sequence = DATABASE_COUNTER.fetch_add(1, Ordering::SeqCst);
    let database_name = format!("finance_v2_test_{}_{}", sequence, Uuid::new_v4().simple());

    let mut admin = PgConnection::connect(admin_database_url)
        .await
        .context("connect to PostgreSQL test administrator database")?;
    admin
        .execute(format!(r#"CREATE DATABASE "{database_name}""#).as_str())
        .await
        .context("create isolated Finance V2 test database")?;
    admin.close().await.ok();

    Ok(FreshV2Database {
        database_url: replace_database_name(admin_database_url, &database_name)?,
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
                "finance_v2_test"
            )
            .unwrap(),
            "postgres://user:password@localhost:5432/finance_v2_test?sslmode=disable"
        );
    }
}
