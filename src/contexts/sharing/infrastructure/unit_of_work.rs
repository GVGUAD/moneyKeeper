//! Sharing transaction boundary.
use sqlx::{PgPool, Postgres, Transaction};
#[derive(Clone)]
pub(crate) struct SharingUnitOfWork {
    pool: PgPool,
}
impl SharingUnitOfWork {
    pub(crate) fn new(pool: PgPool) -> Self {
        Self { pool }
    }
    pub(crate) async fn begin(&self) -> Result<Transaction<'_, Postgres>, sqlx::Error> {
        self.pool.begin().await
    }
}
