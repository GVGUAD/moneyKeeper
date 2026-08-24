//! Provider-neutral Banking bounded context.

pub(crate) mod api;
mod application;
mod domain;
mod infrastructure;
pub mod public;

/// Concrete outer adapters kept separate from Banking's published language.
pub mod adapters {
    pub use super::infrastructure::{Aes256CredentialCipher, MonobankAdapter, MonobankClient};
}

use std::sync::Arc;

use crate::infrastructure::database::VerifiedDatabase;

pub fn build_with_adapters(
    pool: &VerifiedDatabase,
    cipher: Arc<dyn application::CredentialCipher>,
    provider: Arc<dyn application::ProviderClient>,
    currencies: crate::contexts::reference_data::public::CurrencyCatalogFacade,
    webhook_lookup_key: [u8; 32],
) -> public::BankingFacade {
    public::BankingFacade::new(
        Arc::new(infrastructure::PgBankingStore::new(pool)),
        cipher,
        provider,
        Arc::new(infrastructure::MonobankAdapter),
        None,
        currencies,
        Arc::new(infrastructure::WebhookSecretManager::new(
            webhook_lookup_key,
        )),
    )
}

pub fn build_with_ledger(
    pool: &VerifiedDatabase,
    cipher: Arc<dyn application::CredentialCipher>,
    provider: Arc<dyn application::ProviderClient>,
    ledger: crate::contexts::ledger::public::LedgerFacade,
    currencies: crate::contexts::reference_data::public::CurrencyCatalogFacade,
    webhook_lookup_key: [u8; 32],
) -> public::BankingFacade {
    public::BankingFacade::new(
        Arc::new(infrastructure::PgBankingStore::new(pool)),
        cipher,
        provider,
        Arc::new(infrastructure::MonobankAdapter),
        Some(ledger),
        currencies,
        Arc::new(infrastructure::WebhookSecretManager::new(
            webhook_lookup_key,
        )),
    )
}

pub fn webhook_router(banking: public::BankingFacade) -> axum::Router {
    api::routes::webhook_router(banking)
}
