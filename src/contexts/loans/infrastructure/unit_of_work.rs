use sqlx::{PgPool, Postgres, Transaction};
#[derive(Clone)]
pub(crate) struct LoansUnitOfWork {
    pool: PgPool,
}
impl LoansUnitOfWork {
    pub(crate) fn new(pool: PgPool) -> Self {
        Self { pool }
    }
    pub(crate) async fn begin(&self) -> Result<Transaction<'_, Postgres>, sqlx::Error> {
        self.pool.begin().await
    }
}
