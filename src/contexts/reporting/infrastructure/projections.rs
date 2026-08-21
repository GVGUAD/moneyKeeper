//! Projection row, consumed event and checkpoint changes share one transaction.
use sqlx::{PgPool, Postgres, Transaction};
#[derive(Clone)]
pub(crate) struct ProjectionUnitOfWork {
    pool: PgPool,
}
impl ProjectionUnitOfWork {
    pub(crate) fn new(pool: PgPool) -> Self {
        Self { pool }
    }
    pub(crate) async fn begin(&self) -> Result<Transaction<'_, Postgres>, sqlx::Error> {
        self.pool.begin().await
    }
}
