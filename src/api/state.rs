//! Cross-context read capabilities needed by the Ledger HTTP adapter.

use crate::contexts::classification::public::{
    CategoryCatalogFacade, ClassificationAutomationFacade,
};
use crate::contexts::ledger::public::LedgerFacade;
use crate::contexts::reference_data::public::CurrencyCatalogFacade;

/// Capabilities required to compose Ledger account details without private SQL.
#[derive(Clone)]
pub(crate) struct LedgerApiState {
    pub(crate) ledger: LedgerFacade,
    pub(crate) currencies: CurrencyCatalogFacade,
    pub(crate) banking: Option<crate::contexts::banking::public::BankingFacade>,
    pub(crate) categories: CategoryCatalogFacade,
    pub(crate) classification: ClassificationAutomationFacade,
}
