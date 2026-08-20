//! Isolated Finance V2 composition root.

use std::sync::Arc;

use axum::Router;
use jsonwebtoken::jwk::JwkSet;

use crate::contexts::banking::public::{Aes256CredentialCipher, BankingFacade, MonobankClient};
use crate::contexts::classification::public::CategoryCatalogFacade;
use crate::contexts::ledger::public::LedgerFacade;
use crate::contexts::preferences::public::PreferencesFacade;
use crate::contexts::reference_data::public::CurrencyCatalogFacade;
use crate::infrastructure::v2_db::VerifiedV2Pool;

/// Public supporting-context capabilities assembled only after V2 lineage
/// verification. Concrete PostgreSQL adapters remain context-private.
#[derive(Clone)]
pub struct SupportingContexts {
    pub currencies: CurrencyCatalogFacade,
    pub categories: CategoryCatalogFacade,
    pub preferences: PreferencesFacade,
    pub ledger: LedgerFacade,
    pub banking: BankingFacade,
}

/// Builds all Phase 1 supporting capabilities from a verified database.
pub fn supporting_contexts(pool: &VerifiedV2Pool) -> SupportingContexts {
    let categories = crate::contexts::classification::build(pool);
    let currencies = crate::contexts::reference_data::build(pool);
    let ledger = crate::contexts::ledger::build_with_categories(pool, categories.clone());
    let banking = crate::contexts::banking::build_with_ledger(
        pool,
        Arc::new(
            Aes256CredentialCipher::new("parallel-v2-banking", [0x42; 32])
                .expect("the static parallel V2 key has the required length"),
        ),
        Arc::new(MonobankClient::new("https://api.monobank.ua")),
        ledger.clone(),
        currencies.clone(),
        [0x24; 32],
    );
    SupportingContexts {
        currencies,
        categories: categories.clone(),
        preferences: crate::contexts::preferences::build(pool),
        ledger,
        banking,
    }
}

/// Builds the isolated Finance V2 supporting-context router.
///
/// This function does not spawn workers and is intentionally unused by
/// `main.rs` before the Phase 8 cutover.
pub fn router(pool: &VerifiedV2Pool, jwks: Arc<JwkSet>) -> Router {
    crate::api::v2::router(supporting_contexts(pool), jwks)
}
