// tests/common/mod.rs
// Shared test utilities and container setup

use testcontainers::runners::AsyncRunner;
use testcontainers::ContainerAsync;
use testcontainers_modules::postgres::Postgres;
use sqlx::PgPool;

/// PostgreSQL container for testing
pub struct TestPostgres {
    _container: ContainerAsync<Postgres>,
    pub pool: PgPool,
    pub url: String,
}
impl TestPostgres {
    /// Start a PostgreSQL container and return connection pool
    pub async fn new() -> Self {
        let container = Postgres::default()
            .start()
            .await
            .expect("Failed to start PostgreSQL container");

        // Get connection info
        let port = container
            .get_host_port_ipv4(5432)
            .await
            .expect("Failed to get host port");
        let url = format!(
            "postgres://postgres:postgres@localhost:{}/postgres",
            port
        );

        // Create connection pool
        let pool = PgPool::connect(&url)
            .await
            .expect("Failed to connect to PostgreSQL");

        // Run migrations
        sqlx::migrate!("src/infrastructure/migrations")
            .run(&pool)
            .await
            .expect("Failed to run migrations");

        Self {
            _container: container,
            pool,
            url,
        }
    }

    /// Clean up database between tests
    pub async fn cleanup(&self) {
        // Truncate all tables (faster than dropping container)
        sqlx::query("TRUNCATE users, posts, comments RESTART IDENTITY CASCADE")
            .execute(&self.pool)
            .await
            .expect("Failed to truncate tables");
    }
}