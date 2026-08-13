//! Immutable double-entry Ledger bounded context.

mod domain;
mod application;
mod infrastructure;
pub(crate) mod api;

pub mod public;

use crate::infrastructure::v2_db::VerifiedV2Pool;

/// Builds the Ledger facade only from a verified Finance V2 pool.
pub fn build(pool: &VerifiedV2Pool) -> public::LedgerFacade {
    public::LedgerFacade::new(
        infrastructure::PgLedgerUnitOfWork::new(pool),
        infrastructure::PgLedgerQueries::new(pool),
        infrastructure::PgLedgerProjection::new(pool),
    )
}

/// Builds Ledger with Classification's public validation contract.
pub fn build_with_categories(
    pool: &VerifiedV2Pool,
    categories: crate::contexts::classification::public::CategoryCatalogFacade,
) -> public::LedgerFacade {
    public::LedgerFacade::new_with_categories(
        infrastructure::PgLedgerUnitOfWork::new(pool),
        infrastructure::PgLedgerQueries::new(pool),
        infrastructure::PgLedgerProjection::new(pool),
        categories,
    )
}
