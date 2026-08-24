//! Aggregate-shaped Portfolio ports.

use crate::contexts::ledger::public::{
    CancelOrReverseCashControlSettlement, ControlAccountResult, EnsureTypedControlAccount,
    InternalAccountingResult, JournalEntryId, LedgerAccountId, RecordCashControlSettlement,
};
use crate::contexts::portfolio::public::PortfolioFacadeError;
use crate::contexts::portfolio::{
    application::{commands::*, queries::*},
    domain::*,
};
use crate::shared_kernel::UserId;
use crate::shared_kernel::{CorrelationId, CurrencyCode};
use async_trait::async_trait;
use rust_decimal::Decimal;
use std::future::Future;

#[async_trait]
pub(crate) trait PortfolioAccountInstrumentRepository: Send + Sync {
    async fn create_instrument(
        &self,
        command: CreateManualOvdpInstrument,
        hash: [u8; 32],
    ) -> Result<PortfolioCommandResult, PortfolioFacadeError>;
    async fn open_account(
        &self,
        command: OpenPortfolioAccount,
        hash: [u8; 32],
    ) -> Result<PortfolioCommandResult, PortfolioFacadeError>;
    async fn change_account(
        &self,
        command: ChangePortfolioAccount,
        scope: &'static str,
        lifecycle: Option<AccountLifecycle>,
        hash: [u8; 32],
    ) -> Result<PortfolioCommandResult, PortfolioFacadeError>;
    async fn accounts(
        &self,
        user: UserId,
    ) -> Result<Vec<PortfolioAccountView>, PortfolioFacadeError>;
    async fn account(
        &self,
        user: UserId,
        id: PortfolioAccountId,
    ) -> Result<Option<PortfolioAccountView>, PortfolioFacadeError>;
    async fn instruments(&self, user: UserId) -> Result<Vec<InstrumentView>, PortfolioFacadeError>;
    async fn instrument(
        &self,
        user: UserId,
        id: InstrumentId,
    ) -> Result<Option<InstrumentView>, PortfolioFacadeError>;
}

#[async_trait]
pub(crate) trait PortfolioTransactionLotRepository: Send + Sync {
    async fn record(
        &self,
        command: RecordPortfolioTransaction,
        hash: [u8; 32],
    ) -> Result<PortfolioCommandResult, PortfolioFacadeError>;
    async fn reverse(
        &self,
        command: ReversePortfolioTransaction,
        hash: [u8; 32],
    ) -> Result<PortfolioCommandResult, PortfolioFacadeError>;
    async fn positions(
        &self,
        user: UserId,
        account: PortfolioAccountId,
    ) -> Result<Vec<PositionView>, PortfolioFacadeError>;
    async fn activity(
        &self,
        user: UserId,
        account: PortfolioAccountId,
    ) -> Result<Vec<PortfolioTransactionView>, PortfolioFacadeError>;
}

#[async_trait]
pub(crate) trait PortfolioValuationRepository: Send + Sync {
    async fn record_valuation(
        &self,
        command: RecordValuationSnapshot,
        hash: [u8; 32],
    ) -> Result<PortfolioCommandResult, PortfolioFacadeError>;
    async fn valuations(
        &self,
        user: UserId,
        account: PortfolioAccountId,
        instrument: InstrumentId,
    ) -> Result<Vec<ValuationView>, PortfolioFacadeError>;
}

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
    fn ensure_typed_control_account(
        &self,
        command: EnsureTypedControlAccount,
    ) -> impl Future<Output = Result<ControlAccountResult, Self::Error>> + Send;
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
    async fn ensure_typed_control_account(
        &self,
        command: EnsureTypedControlAccount,
    ) -> Result<ControlAccountResult, Self::Error> {
        self.ensure_typed_control_account(command).await
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CashSettlementAction {
    Post,
    CancelOrReverse,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CashSettlementState {
    Retrying,
    Posted,
    Failed,
    CancelledNoFinancialEffect,
    Reversed,
}

#[derive(Clone, Debug)]
pub(crate) struct CashSettlementWork {
    pub transaction_id: PortfolioTransactionId,
    pub user_id: UserId,
    pub cash_account_id: LedgerAccountId,
    pub amount: Decimal,
    pub currency: CurrencyCode,
    pub cash_flow: crate::contexts::ledger::public::CashFlowDirection,
    pub correlation_id: CorrelationId,
    pub action: CashSettlementAction,
    pub journal_id: Option<JournalEntryId>,
    pub reversal_journal_id: Option<JournalEntryId>,
}

#[derive(Clone, Debug)]
pub(crate) struct CashSettlementCompletion {
    pub work: CashSettlementWork,
    pub state: CashSettlementState,
    pub journal_id: Option<JournalEntryId>,
    pub reversal_journal_id: Option<JournalEntryId>,
    pub last_error: Option<String>,
}

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub(crate) struct CashSettlementServiceError {
    message: &'static str,
    #[source]
    source: Option<Box<dyn std::error::Error + Send + Sync>>,
}

impl CashSettlementServiceError {
    pub(crate) fn persistence(source: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self {
            message: "Portfolio cash-settlement persistence failed",
            source: Some(Box::new(source)),
        }
    }
    pub(crate) fn ledger(source: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self {
            message: "Portfolio cash settlement was rejected by Ledger",
            source: Some(Box::new(source)),
        }
    }
    pub(crate) fn invalid(source: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self {
            message: "Portfolio cash-settlement state is invalid",
            source: Some(Box::new(source)),
        }
    }
}

#[async_trait]
pub(crate) trait PortfolioCashSettlementRepository: Clone + Send + Sync + 'static {
    async fn claim_next(&self) -> Result<Option<CashSettlementWork>, CashSettlementServiceError>;
    async fn complete(
        &self,
        completion: CashSettlementCompletion,
    ) -> Result<(), CashSettlementServiceError>;
}
