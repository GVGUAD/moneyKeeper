//! Reference Data owns the enabled ISO currency catalog.

mod application;
mod domain;
pub(crate) mod infrastructure;

pub(crate) mod api;
pub mod public;

use crate::infrastructure::database::VerifiedDatabase;
use std::sync::Arc;

pub(crate) fn build(pool: &VerifiedDatabase) -> public::CurrencyCatalogFacade {
    public::CurrencyCatalogFacade::new(
        Arc::new(infrastructure::PgCurrencyCatalog::new(pool.pool().clone())),
        Arc::new(infrastructure::fx_repository::PgFxRepository::new(
            pool.pool().clone(),
        )),
    )
}
