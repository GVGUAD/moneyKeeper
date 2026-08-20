//! State shared by the isolated Finance V2 Ledger HTTP adapter.

use crate::contexts::ledger::public::LedgerFacade;
use crate::contexts::reference_data::public::CurrencyCatalogFacade;

/// Capabilities required by Ledger HTTP handlers.
#[derive(Clone)]
pub(crate) struct LedgerApiState {
    pub(crate) ledger: LedgerFacade,
    pub(crate) currencies: CurrencyCatalogFacade,
    pub(crate) banking: Option<crate::contexts::banking::public::BankingFacade>,
}
