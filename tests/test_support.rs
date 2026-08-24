use std::sync::{Arc, Weak};

use sqlx::PgPool;
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, ImageExt};
use testcontainers_modules::postgres::Postgres;
use tokio::sync::Mutex;

use moneykeeper::infrastructure::database::VerifiedDatabase;
use moneykeeper::infrastructure::test_db::{FreshDatabase, create_fresh_database};

static CONTAINER: Mutex<Option<Weak<SharedPostgres>>> = Mutex::const_new(None);

struct SharedPostgres {
    _container: ContainerAsync<Postgres>,
    admin_url: String,
}

async fn postgres() -> Arc<SharedPostgres> {
    let mut shared = CONTAINER.lock().await;
    if let Some(container) = shared.as_ref().and_then(Weak::upgrade) {
        return container;
    }
    let container = Postgres::default()
        .with_tag("16-alpine")
        .with_startup_timeout(std::time::Duration::from_secs(120))
        .start()
        .await
        .expect("start PostgreSQL 16 testcontainer");
    let port = container
        .get_host_port_ipv4(5432)
        .await
        .expect("resolve PostgreSQL test port");
    let container = Arc::new(SharedPostgres {
        _container: container,
        admin_url: format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres"),
    });
    *shared = Some(Arc::downgrade(&container));
    container
}

#[allow(dead_code)]
pub async fn fresh_pool() -> VerifiedDatabase {
    let database = fresh_database().await;
    database
        .initialize()
        .await
        .expect("initialize Moneykeeper database")
}

#[allow(dead_code)]
pub async fn fresh_runtime() -> (VerifiedDatabase, PgPool) {
    let database = fresh_database().await;
    let verified = database
        .initialize()
        .await
        .expect("initialize Moneykeeper database");
    let pool = database
        .connect()
        .await
        .expect("connect explicit test SQL pool");
    (verified, pool)
}

pub async fn fresh_database() -> FreshDatabase {
    let postgres = postgres().await;
    create_fresh_database(&postgres.admin_url)
        .await
        .expect("create isolated Moneykeeper database")
        .with_lifetime_guard(postgres)
}
