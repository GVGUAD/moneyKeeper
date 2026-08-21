//! Contacts-first shared-bill bounded context.
#![allow(dead_code)]

pub(crate) mod api;
pub(crate) mod application;
pub mod domain;
pub(crate) mod infrastructure;
pub mod public;

use crate::infrastructure::v2_db::VerifiedV2Pool;

pub(crate) fn build(pool: &VerifiedV2Pool) -> public::SharingFacade {
    public::SharingFacade::new(infrastructure::PgSharingStore::new(pool.pool().clone()))
}
