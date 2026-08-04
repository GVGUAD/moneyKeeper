use std::sync::atomic::{AtomicU64, Ordering};

use sqlx::{Connection, Executor, PgConnection, PgPool};
use testcontainers::ContainerAsync;
use testcontainers::runners::AsyncRunner;
use testcontainers_modules::postgres::Postgres;
use tokio::sync::OnceCell;

static CONTAINER: OnceCell<SharedContainer> = OnceCell::const_new();
static DB_COUNTER: AtomicU64 = AtomicU64::new(0);

struct SharedContainer {
    _container: ContainerAsync<Postgres>,
    host_port: u16,
}

async fn shared_container() -> &'static SharedContainer {
    CONTAINER
        .get_or_init(|| async {
            let container = Postgres::default()
                .start()
                .await
                .expect("failed to start postgres testcontainer");
            let host_port = container
                .get_host_port_ipv4(5432)
                .await
                .expect("failed to get postgres host port");
            SharedContainer {
                _container: container,
                host_port,
            }
        })
        .await
}

fn admin_url(port: u16) -> String {
    format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres")
}

fn db_url(port: u16, db: &str) -> String {
    format!("postgres://postgres:postgres@127.0.0.1:{port}/{db}")
}

pub async fn fresh_pool() -> PgPool {
    let shared = shared_container().await;
    let port = shared.host_port;

    let n = DB_COUNTER.fetch_add(1, Ordering::SeqCst);
    let db_name = format!("test_db_{n}");

    let mut admin = PgConnection::connect(&admin_url(port))
        .await
        .expect("connect to admin postgres");
    admin
        .execute(format!(r#"CREATE DATABASE "{db_name}""#).as_str())
        .await
        .expect("create test database");
    admin.close().await.ok();

    let pool = PgPool::connect(&db_url(port, &db_name))
        .await
        .expect("connect to test database");
    sqlx::migrate!("src/infrastructure/migrations")
        .run(&pool)
        .await
        .expect("run migrations on test database");
    pool
}
