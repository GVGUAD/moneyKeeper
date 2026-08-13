//! Classification owns user category taxonomy and lifecycle.

mod application;
mod domain;
mod infrastructure;

pub(crate) mod api;
pub mod public;

use crate::infrastructure::v2_db::VerifiedV2Pool;

pub(crate) fn build(pool: &VerifiedV2Pool) -> public::CategoryCatalogFacade {
    public::CategoryCatalogFacade::new(infrastructure::PgCategoryCatalog::new(pool.pool().clone()))
}
