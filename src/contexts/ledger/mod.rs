//! Immutable double-entry Ledger bounded context.

pub(crate) mod api;
mod application;
mod domain;
mod infrastructure;

mod analytics;
pub mod public;

use crate::infrastructure::database::VerifiedDatabase;
use std::sync::Arc;

/// Builds the Ledger facade only from a verified Moneykeeper pool.
pub fn build(pool: &VerifiedDatabase) -> public::LedgerFacade {
    let application = application::accounts::LedgerApplication::new(
        infrastructure::PgLedgerUnitOfWork::new(pool),
        infrastructure::PgLedgerQueries::new(pool),
        infrastructure::PgLedgerProjection::new(pool),
    );
    public::LedgerFacade::new(Arc::new(application))
}

/// Builds Ledger with Classification's public validation contract.
pub fn build_with_categories(
    pool: &VerifiedDatabase,
    categories: crate::contexts::classification::public::CategoryCatalogFacade,
) -> public::LedgerFacade {
    let application = application::accounts::LedgerApplication::new_with_categories(
        infrastructure::PgLedgerUnitOfWork::new(pool),
        infrastructure::PgLedgerQueries::new(pool),
        infrastructure::PgLedgerProjection::new(pool),
        categories,
    );
    public::LedgerFacade::new(Arc::new(application))
}

mod conversion;
