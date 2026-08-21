//! Rebuildable financial Reporting context.
#![allow(dead_code)]
pub(crate) mod api;
pub(crate) mod application;
pub(crate) mod infrastructure;
pub mod public;
use crate::infrastructure::v2_db::VerifiedV2Pool;
pub(crate) fn build(pool: &VerifiedV2Pool) -> public::ReportingFacade {
    public::ReportingFacade::new(infrastructure::PgReportingStore::new(pool.pool().clone()))
}
