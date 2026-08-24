//! Rebuildable financial Reporting context.
#![allow(dead_code)]
pub(crate) mod api;
pub(crate) mod application;
pub(crate) mod infrastructure;
pub mod public;
use crate::infrastructure::database::VerifiedDatabase;
use std::sync::Arc;
pub(crate) fn build(pool: &VerifiedDatabase) -> public::ReportingFacade {
    let repository = Arc::new(infrastructure::PgReportingStore::new(pool.pool().clone()));
    public::ReportingFacade::new(repository.clone(), repository.clone(), repository)
}
