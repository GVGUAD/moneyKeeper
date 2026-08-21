//! Borrowed and lent agreement lifecycle bounded context.
#![allow(dead_code)]

pub(crate) mod api;
pub(crate) mod application;
pub mod domain;
pub(crate) mod infrastructure;
pub mod public;

use crate::infrastructure::v2_db::VerifiedV2Pool;

pub(crate) fn build(pool: &VerifiedV2Pool) -> public::LoansFacade {
    public::LoansFacade::new(infrastructure::PgLoansStore::new(pool.pool().clone()))
}
