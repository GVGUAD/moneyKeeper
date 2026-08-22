//! Aggregate-shaped Portfolio ports.

use crate::contexts::ledger::public::{
    CancelOrReverseCashControlSettlement, InternalAccountingResult, RecordCashControlSettlement,
};
use std::future::Future;

pub trait PortfolioLedger: Clone + Send + Sync + 'static {
    type Error: std::error::Error + Send + Sync + 'static;
    fn record_cash_control_settlement(
        &self,
        command: RecordCashControlSettlement,
    ) -> impl Future<Output = Result<InternalAccountingResult, Self::Error>> + Send;
    fn cancel_or_reverse_cash_control_settlement(
        &self,
        command: CancelOrReverseCashControlSettlement,
    ) -> impl Future<Output = Result<InternalAccountingResult, Self::Error>> + Send;
}

impl PortfolioLedger for crate::contexts::ledger::public::LedgerFacade {
    type Error = crate::contexts::ledger::public::LedgerError;
    async fn record_cash_control_settlement(
        &self,
        command: RecordCashControlSettlement,
    ) -> Result<InternalAccountingResult, Self::Error> {
        self.record_cash_control_settlement(command).await
    }
    async fn cancel_or_reverse_cash_control_settlement(
        &self,
        command: CancelOrReverseCashControlSettlement,
    ) -> Result<InternalAccountingResult, Self::Error> {
        self.cancel_or_reverse_cash_control_settlement(command)
            .await
    }
}
