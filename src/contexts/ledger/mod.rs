//! Immutable double-entry Ledger bounded context.

mod domain;
mod application;
mod infrastructure;

pub mod public;

use crate::infrastructure::v2_db::VerifiedV2Pool;

/// Builds the Ledger facade only from a verified Finance V2 pool.
pub fn build(pool: &VerifiedV2Pool) -> public::LedgerFacade {
    public::LedgerFacade::new(infrastructure::PgLedgerUnitOfWork::new(pool))
}
