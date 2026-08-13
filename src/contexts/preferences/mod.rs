//! Preferences owns each user's reporting base currency.

mod application;
mod domain;
mod infrastructure;

pub(crate) mod api;
pub mod public;

use crate::infrastructure::v2_db::VerifiedV2Pool;

pub(crate) fn build(pool: &VerifiedV2Pool) -> public::PreferencesFacade {
    public::PreferencesFacade::new(infrastructure::PgPreferences::new(pool.pool().clone()))
}
