//! Reference Data owns the enabled ISO currency catalog.

mod application;
mod domain;
pub(crate) mod infrastructure;

pub(crate) mod api;
pub mod public;

use crate::infrastructure::v2_db::VerifiedV2Pool;

pub(crate) fn build(pool: &VerifiedV2Pool) -> public::CurrencyCatalogFacade {
    public::CurrencyCatalogFacade::new(
        infrastructure::PgCurrencyCatalog::new(pool.pool().clone()),
        infrastructure::fx_repository::PgFxRepository::new(pool.pool().clone()),
    )
}
