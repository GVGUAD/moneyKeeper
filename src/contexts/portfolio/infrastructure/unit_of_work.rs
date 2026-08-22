//! Portfolio SQLx unit of work.
use sqlx::{PgPool, Postgres, Transaction};
#[derive(Clone)]
pub(crate) struct PortfolioUnitOfWork {
    pool: PgPool,
}
impl PortfolioUnitOfWork {
    pub(crate) fn new(pool: PgPool) -> Self {
        Self { pool }
    }
    pub(crate) async fn begin(&self) -> Result<Transaction<'_, Postgres>, sqlx::Error> {
        self.pool.begin().await
    }
}
