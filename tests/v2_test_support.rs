use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, ImageExt};
use testcontainers_modules::postgres::Postgres;
use tokio::sync::OnceCell;

use moneykeeper::infrastructure::v2_db::VerifiedV2Pool;
use moneykeeper::infrastructure::v2_test_db::{FreshV2Database, create_fresh_database};

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

#[allow(dead_code)]
pub async fn fresh_v2_pool() -> VerifiedV2Pool {
    let database = fresh_v2_database().await;
    database
        .initialize()
        .await
        .expect("initialize Finance V2 database")
}

#[allow(dead_code)]
pub async fn fresh_v2_runtime() -> (VerifiedV2Pool, PgPool) {
    let database = fresh_v2_database().await;
    let verified = database
        .initialize()
        .await
        .expect("initialize Finance V2 database");
    let pool = PgPool::connect(database.database_url())
        .await
        .expect("connect explicit test SQL pool");
    (verified, pool)
}

pub async fn fresh_v2_database() -> FreshV2Database {
    create_fresh_database(&postgres().await.admin_url)
        .await
        .expect("create isolated Finance V2 database")
}
use sqlx::PgPool;
