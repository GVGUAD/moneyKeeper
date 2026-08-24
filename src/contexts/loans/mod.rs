//! Borrowed and lent agreement lifecycle bounded context.
#![allow(dead_code)]

pub(crate) mod api;
pub(crate) mod application;
pub mod domain;
pub(crate) mod infrastructure;
pub mod public;

use crate::infrastructure::database::VerifiedDatabase;
use std::sync::Arc;

pub(crate) fn build(pool: &VerifiedDatabase) -> public::LoansFacade {
    let repository = Arc::new(infrastructure::PgLoansStore::new(pool.pool().clone()));
    public::LoansFacade::new(repository.clone(), repository.clone(), repository)
}
