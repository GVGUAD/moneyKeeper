//! Mail transactions atomically persist receipts, facts and outbox messages.
use sqlx::{PgPool, Postgres, Transaction};
#[derive(Clone)]
pub(crate) struct MailUnitOfWork {
    pool: PgPool,
}
impl MailUnitOfWork {
    pub(crate) fn new(pool: PgPool) -> Self {
        Self { pool }
    }
    pub(crate) async fn begin(&self) -> Result<Transaction<'_, Postgres>, sqlx::Error> {
        self.pool.begin().await
    }
}
