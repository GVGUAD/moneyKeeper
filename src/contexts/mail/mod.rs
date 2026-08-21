//! Mail owns encrypted connections and immutable receipt evidence.
#![allow(dead_code, unused_imports)]

pub(crate) mod api;
pub(crate) mod application;
pub mod domain;
pub(crate) mod infrastructure;
pub mod public;

use crate::infrastructure::v2_db::VerifiedV2Pool;
pub(crate) fn build(pool: &VerifiedV2Pool) -> public::MailFacade {
    public::MailFacade::new(
        infrastructure::PgMailStore::new(pool.pool().clone()),
        std::sync::Arc::new(infrastructure::oauth::GoogleOAuthClient::from_environment()),
    )
}
