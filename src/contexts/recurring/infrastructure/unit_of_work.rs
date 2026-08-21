use sqlx::{PgPool, Postgres, Transaction};
#[derive(Clone)]
pub(crate) struct RecurringUnitOfWork {
    pool: PgPool,
}
impl RecurringUnitOfWork {
    pub(crate) fn new(pool: PgPool) -> Self {
        Self { pool }
    }
    pub(crate) async fn begin(&self) -> Result<Transaction<'_, Postgres>, sqlx::Error> {
        self.pool.begin().await
    }
}
