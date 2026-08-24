//! Preferences owns each user's reporting base currency.

mod application;
mod domain;
mod infrastructure;

pub(crate) mod api;
pub mod public;

use crate::infrastructure::database::VerifiedDatabase;
use std::sync::Arc;

pub(crate) fn build(pool: &VerifiedDatabase) -> public::PreferencesFacade {
    public::PreferencesFacade::new(Arc::new(infrastructure::PgPreferences::new(
        pool.pool().clone(),
    )))
}
