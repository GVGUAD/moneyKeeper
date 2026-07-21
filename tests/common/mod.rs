// tests/common/mod.rs
// Shared test utilities and container setup

use sqlx::PgPool;
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, ImageExt};
use testcontainers_modules::postgres::Postgres;

/// PostgreSQL container for testing
pub struct TestPostgres {
    _container: ContainerAsync<Postgres>,
    pub pool: PgPool,
}

impl TestPostgres {
    /// Start a PostgreSQL container and return connection pool
    pub async fn new() -> Self {
        let container = Postgres::default()
            .with_tag("16-alpine")
            .start()
            .await
            .expect("Failed to start PostgreSQL container");

        let port = container
            .get_host_port_ipv4(5432)
            .await
            .expect("Failed to get host port");
        let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");

        let pool = PgPool::connect(&url)
            .await
            .expect("Failed to connect to PostgreSQL");

        sqlx::migrate!("src/infrastructure/migrations")
            .run(&pool)
            .await
            .expect("Failed to run migrations");

        Self {
            _container: container,
            pool,
        }
    }
}
