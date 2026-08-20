//! Provider-neutral Banking bounded context.

mod domain;
mod application;
mod infrastructure;
pub mod public;

use std::sync::Arc;

use crate::infrastructure::v2_db::VerifiedV2Pool;

pub fn build_with_adapters(
    pool: &VerifiedV2Pool,
    cipher: Arc<dyn application::CredentialCipher>,
    provider: Arc<dyn application::ProviderClient>,
) -> public::BankingFacade {
    public::BankingFacade::new(infrastructure::PgBankingStore::new(pool), cipher, provider)
}
