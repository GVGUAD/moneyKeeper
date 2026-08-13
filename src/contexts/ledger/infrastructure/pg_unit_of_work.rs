//! Concrete PostgreSQL Ledger unit of work.

use sqlx::{PgPool, Postgres, Transaction};

use crate::infrastructure::v2_db::VerifiedV2Pool;

use super::super::{
    application::ports::{LedgerUnitOfWork, TransactionControl},
    domain::LedgerError,
};

/// Begins transaction-bound Ledger aggregate stores.
#[derive(Clone)]
pub(crate) struct PgLedgerUnitOfWork {
    pool: PgPool,
}

impl PgLedgerUnitOfWork {
    pub(crate) fn new(pool: &VerifiedV2Pool) -> Self {
        Self { pool: pool.pool().clone() }
    }
}

/// All write adapters borrow this exact transaction.
pub(crate) struct PgLedgerTransaction<'a> {
    pub(super) transaction: Transaction<'a, Postgres>,
}

impl LedgerUnitOfWork for PgLedgerUnitOfWork {
    type Tx<'a> = PgLedgerTransaction<'a>;

    async fn begin(&self) -> Result<Self::Tx<'_>, LedgerError> {
        Ok(PgLedgerTransaction {
            transaction: self.pool.begin().await.map_err(LedgerError::database)?,
        })
    }
}

impl TransactionControl for PgLedgerTransaction<'_> {
    async fn commit(self) -> Result<(), LedgerError> {
        self.transaction.commit().await.map_err(LedgerError::database)
    }

    async fn rollback(self) -> Result<(), LedgerError> {
        self.transaction.rollback().await.map_err(LedgerError::database)
    }
}
