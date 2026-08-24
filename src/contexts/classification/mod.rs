//! Classification owns user category taxonomy and lifecycle.

mod application;
mod domain;
mod infrastructure;

pub(crate) mod api;
pub mod public;

use crate::infrastructure::database::VerifiedDatabase;
use std::sync::Arc;

pub(crate) fn build(pool: &VerifiedDatabase) -> public::CategoryCatalogFacade {
    public::CategoryCatalogFacade::new(Arc::new(infrastructure::PgCategoryCatalog::new(
        pool.pool().clone(),
    )))
}
