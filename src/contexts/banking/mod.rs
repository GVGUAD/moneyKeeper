//! Provider-neutral Banking bounded context.

mod domain;
mod application;
mod infrastructure;
pub(crate) mod api;
pub mod public;

use std::sync::Arc;

use crate::infrastructure::v2_db::VerifiedV2Pool;

pub fn build_with_adapters(
    pool: &VerifiedV2Pool,
    cipher: Arc<dyn application::CredentialCipher>,
    provider: Arc<dyn application::ProviderClient>,
    currencies: crate::contexts::reference_data::public::CurrencyCatalogFacade,
    webhook_lookup_key: [u8; 32],
) -> public::BankingFacade {
    public::BankingFacade::new(infrastructure::PgBankingStore::new(pool), cipher, provider, None, currencies, infrastructure::WebhookSecretManager::new(webhook_lookup_key))
}

pub fn build_with_ledger(
    pool: &VerifiedV2Pool,
    cipher: Arc<dyn application::CredentialCipher>,
    provider: Arc<dyn application::ProviderClient>,
    ledger: crate::contexts::ledger::public::LedgerFacade,
    currencies: crate::contexts::reference_data::public::CurrencyCatalogFacade,
    webhook_lookup_key: [u8; 32],
) -> public::BankingFacade {
    public::BankingFacade::new(infrastructure::PgBankingStore::new(pool), cipher, provider, Some(ledger), currencies, infrastructure::WebhookSecretManager::new(webhook_lookup_key))
}

pub fn webhook_router(banking: public::BankingFacade) -> axum::Router { api::routes::webhook_router(banking) }
