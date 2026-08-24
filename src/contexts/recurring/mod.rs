//! Recurring inventory and charge-matching bounded context.
#![allow(dead_code)]
pub(crate) mod api;
pub(crate) mod application;
pub mod domain;
pub(crate) mod infrastructure;
pub mod public;
use crate::infrastructure::database::VerifiedDatabase;
use std::sync::Arc;
pub(crate) fn build(pool: &VerifiedDatabase) -> public::RecurringFacade {
    public::RecurringFacade::new(Arc::new(infrastructure::PgRecurringStore::new(
        pool.pool().clone(),
    )))
}
