//! Transaction boundary reserved for Banking-owned writes.

use sqlx::PgPool;

#[derive(Clone)]
pub(super) struct PgBankingUnitOfWork {
    pub(super) pool: PgPool,
}
