//! Narrow application ports for cross-context accounting.

use async_trait::async_trait;
use std::future::Future;

use crate::contexts::ledger::public::{
    ControlAccountResult, EnsureTypedControlAccount, InternalAccountingResult,
};
use crate::contexts::ledger::public::{JournalEntryId, LedgerAccountId};
use crate::contexts::loans::domain::{LoanAgreementId, LoanMovementId};
use crate::contexts::loans::public::{LoanAccountingCommand, LoanOpeningCommand};
use crate::contexts::loans::public::{
    LoanCommandResult, LoanEventV1, LoanMovementView, LoanView, LoansError, OpenLoan,
    PendingLoanMovement, PendingLoanReplacement, PendingLoanReversal, RecordLoanMovement,
    RequestLoanReversal, ReviseLoanTerms,
};
use crate::shared_kernel::{CorrelationId, UserId};
use chrono::{DateTime, Utc};

#[async_trait]
pub(crate) trait LoanAgreementRepository: Send + Sync {
    async fn open(
        &self,
        command: OpenLoan,
        hash: [u8; 32],
    ) -> Result<LoanCommandResult, LoansError>;
    async fn revise(
        &self,
        command: ReviseLoanTerms,
        hash: [u8; 32],
    ) -> Result<LoanCommandResult, LoansError>;
    #[allow(clippy::too_many_arguments)]
    async fn close(
        &self,
        user: UserId,
        id: LoanAgreementId,
        expected: u64,
        key: &str,
        hash: [u8; 32],
        correlation: CorrelationId,
        now: DateTime<Utc>,
    ) -> Result<LoanCommandResult, LoansError>;
    async fn list(&self, user: UserId) -> Result<Vec<LoanView>, LoansError>;
    async fn get(&self, user: UserId, id: LoanAgreementId) -> Result<Option<LoanView>, LoansError>;
    async fn term_revisions(
        &self,
        user: UserId,
        id: LoanAgreementId,
    ) -> Result<Vec<serde_json::Value>, LoansError>;
}

#[async_trait]
pub(crate) trait LoanMovementRepository: Send + Sync {
    async fn record_movement(
        &self,
        command: RecordLoanMovement,
        hash: [u8; 32],
    ) -> Result<LoanCommandResult, LoansError>;
    async fn movements(
        &self,
        user: UserId,
        id: LoanAgreementId,
    ) -> Result<Vec<LoanMovementView>, LoansError>;
    async fn movement(
        &self,
        user: UserId,
        agreement: LoanAgreementId,
        movement: LoanMovementId,
    ) -> Result<Option<LoanMovementView>, LoansError>;
    async fn request_reversal(
        &self,
        command: RequestLoanReversal,
        hash: [u8; 32],
    ) -> Result<LoanCommandResult, LoansError>;
    async fn request_replacement(
        &self,
        command: RecordLoanMovement,
        original: LoanMovementId,
        hash: [u8; 32],
    ) -> Result<LoanCommandResult, LoansError>;
}

#[async_trait]
pub(crate) trait LoanAccountingWorkflowRepository: Send + Sync {
    async fn pending_openings(&self, limit: i64) -> Result<Vec<LoanView>, LoansError>;
    async fn confirm_opening(
        &self,
        user: UserId,
        id: LoanAgreementId,
        account: LedgerAccountId,
        now: DateTime<Utc>,
    ) -> Result<(), LoansError>;
    async fn fail_opening(
        &self,
        user: UserId,
        id: LoanAgreementId,
        error: &str,
        now: DateTime<Utc>,
    ) -> Result<(), LoansError>;
    async fn pending_movements(&self, limit: i64) -> Result<Vec<PendingLoanMovement>, LoansError>;
    async fn confirm_movement(
        &self,
        user: UserId,
        agreement: LoanAgreementId,
        movement: LoanMovementId,
        journal: JournalEntryId,
        now: DateTime<Utc>,
    ) -> Result<LoanEventV1, LoansError>;
    async fn fail_movement(
        &self,
        user: UserId,
        agreement: LoanAgreementId,
        movement: LoanMovementId,
        error: &str,
        now: DateTime<Utc>,
    ) -> Result<(), LoansError>;
    async fn pending_reversals(&self, limit: i64) -> Result<Vec<PendingLoanReversal>, LoansError>;
    async fn confirm_reversal(
        &self,
        pending: &PendingLoanReversal,
        reversal: JournalEntryId,
        now: DateTime<Utc>,
    ) -> Result<LoanEventV1, LoansError>;
    async fn pending_replacements(
        &self,
        limit: i64,
    ) -> Result<Vec<PendingLoanReplacement>, LoansError>;
    async fn confirm_replacement_reversal(
        &self,
        pending: &PendingLoanReplacement,
        reversal: JournalEntryId,
        now: DateTime<Utc>,
    ) -> Result<(), LoansError>;
}

/// Ledger capabilities required by Loans process managers.
pub trait LoanLedger: Clone + Send + Sync + 'static {
    fn open_loan_account(
        &self,
        command: LoanOpeningCommand,
    ) -> impl Future<
        Output = Result<
            crate::contexts::ledger::public::AccountResult,
            crate::contexts::ledger::public::LedgerError,
        >,
    > + Send;
    fn ensure_accrual_account(
        &self,
        command: EnsureTypedControlAccount,
    ) -> impl Future<
        Output = Result<ControlAccountResult, crate::contexts::ledger::public::LedgerError>,
    > + Send;
    fn post_loan_accounting(
        &self,
        command: LoanAccountingCommand,
    ) -> impl Future<
        Output = Result<InternalAccountingResult, crate::contexts::ledger::public::LedgerError>,
    > + Send;
}
