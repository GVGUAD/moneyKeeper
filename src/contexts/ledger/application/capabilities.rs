//! Object-safe capabilities wrapped by the concrete public Ledger facade.

use async_trait::async_trait;

use crate::shared_kernel::UserId;

use super::{
    accounts::LedgerApplication,
    ports::{LedgerQueryPort, LedgerUnitOfWork, ProjectionRebuildPort},
};
use crate::contexts::ledger::public::*;

#[async_trait]
pub(crate) trait LedgerCommandCapability: Send + Sync {
    async fn open_account(&self, command: OpenAccount) -> Result<AccountResult, LedgerError>;
    async fn open_provider_observed_account(
        &self,
        command: OpenProviderObservedAccount,
    ) -> Result<AccountResult, LedgerError>;
    async fn rename_account(&self, command: RenameAccount) -> Result<AccountResult, LedgerError>;
    async fn archive_account(&self, command: ArchiveAccount) -> Result<AccountResult, LedgerError>;
    async fn restore_account(&self, command: RestoreAccount) -> Result<AccountResult, LedgerError>;
    async fn record_manual_transaction(
        &self,
        command: RecordManualTransaction,
    ) -> Result<TransactionResult, LedgerError>;
    async fn transfer(&self, command: TransferFunds) -> Result<TransferResult, LedgerError>;
    async fn update_annotation(
        &self,
        command: UpdateTransactionAnnotation,
    ) -> Result<AnnotationResult, LedgerError>;
    async fn apply_category_assignment(
        &self,
        command: ApplyCategoryAssignment,
    ) -> Result<CategoryAssignmentResult, LedgerError>;
    async fn restore_category_assignment(
        &self,
        command: RestoreCategoryAssignment,
    ) -> Result<CategoryAssignmentResult, LedgerError>;
    async fn enable_automatic_classification(
        &self,
        command: EnableAutomaticClassification,
    ) -> Result<CategoryAssignmentResult, LedgerError>;
    async fn correct_balance(
        &self,
        command: CorrectBalance,
    ) -> Result<FinancialChangeResult, LedgerError>;
    async fn reverse_transaction(
        &self,
        command: ReverseTransaction,
    ) -> Result<FinancialChangeResult, LedgerError>;
    async fn replace_transaction(
        &self,
        command: ReplaceTransaction,
    ) -> Result<ReplacementResult, LedgerError>;
}

#[async_trait]
pub(crate) trait LedgerQueryCapability: Send + Sync {
    async fn validate_provider_account_binding(
        &self,
        command: ValidateProviderAccountBinding,
    ) -> Result<ProviderAccountBindingResult, LedgerError>;
    async fn list_accounts(&self, user_id: UserId) -> Result<Vec<AccountView>, LedgerError>;
    async fn get_account(
        &self,
        user_id: UserId,
        id: LedgerAccountId,
    ) -> Result<AccountView, LedgerError>;
    async fn account_activity(
        &self,
        user_id: UserId,
        account_id: LedgerAccountId,
        after: Option<ActivityCursor>,
        limit: u32,
    ) -> Result<Vec<JournalView>, LedgerError>;
    async fn list_journals(
        &self,
        user_id: UserId,
        after: Option<ActivityCursor>,
        limit: u32,
    ) -> Result<Vec<JournalView>, LedgerError>;
    async fn list_activity(
        &self,
        user_id: UserId,
        filter: ActivityFilter,
        after: Option<ActivityCursor>,
        limit: u32,
    ) -> Result<Vec<JournalView>, LedgerError>;
    async fn summarize_activity(
        &self,
        user_id: UserId,
        filter: ActivityFilter,
    ) -> Result<ActivitySummary, LedgerError>;
    async fn get_journal(
        &self,
        user_id: UserId,
        id: JournalEntryId,
    ) -> Result<JournalView, LedgerError>;
    async fn verify_projection(&self) -> Result<Vec<ProjectionMismatch>, LedgerError>;
    async fn rebuild_projection(&self) -> Result<(), LedgerError>;
}

#[async_trait]
pub(crate) trait LedgerReconciliationCapability: Send + Sync {
    async fn observe_provider_balance(
        &self,
        command: ObserveProviderBalance,
    ) -> Result<ReconciliationResult, LedgerError>;
    async fn approve_reconciliation(
        &self,
        command: ApproveReconciliation,
    ) -> Result<ReconciliationResult, LedgerError>;
    async fn dismiss_reconciliation(
        &self,
        command: DismissReconciliation,
    ) -> Result<ReconciliationResult, LedgerError>;
    async fn list_reconciliations(
        &self,
        user_id: UserId,
    ) -> Result<Vec<ReconciliationView>, LedgerError>;
    async fn get_reconciliation(
        &self,
        user_id: UserId,
        id: ReconciliationCaseId,
    ) -> Result<ReconciliationView, LedgerError>;
}

#[async_trait]
pub(crate) trait LedgerInternalAccountingCapability: Send + Sync {
    async fn ensure_typed_control_account(
        &self,
        command: EnsureTypedControlAccount,
    ) -> Result<ControlAccountResult, LedgerError>;
    async fn record_expense_and_control_balances(
        &self,
        command: RecordExpenseAndControlBalances,
    ) -> Result<InternalAccountingResult, LedgerError>;
    async fn import_provider_transaction(
        &self,
        command: ImportProviderTransaction,
    ) -> Result<InternalAccountingResult, LedgerError>;
    async fn transition_provider_transaction_state(
        &self,
        command: TransitionProviderTransactionState,
    ) -> Result<InternalAccountingResult, LedgerError>;
    async fn reverse_provider_transaction(
        &self,
        command: ReverseProviderTransaction,
    ) -> Result<InternalAccountingResult, LedgerError>;
    async fn settle_receivable_or_payable(
        &self,
        command: SettleReceivableOrPayable,
    ) -> Result<InternalAccountingResult, LedgerError>;
    async fn record_principal_disbursement(
        &self,
        command: RecordPrincipalDisbursement,
    ) -> Result<InternalAccountingResult, LedgerError>;
    async fn record_principal_repayment(
        &self,
        command: RecordPrincipalRepayment,
    ) -> Result<InternalAccountingResult, LedgerError>;
    async fn record_interest_and_fee(
        &self,
        command: RecordInterestAndFee,
    ) -> Result<InternalAccountingResult, LedgerError>;
    async fn record_interest_or_fee_accrual(
        &self,
        command: RecordInterestOrFeeAccrual,
    ) -> Result<InternalAccountingResult, LedgerError>;
    async fn reclassify_expense_to_receivable_or_payable(
        &self,
        command: ReclassifyExpenseToReceivableOrPayable,
    ) -> Result<InternalAccountingResult, LedgerError>;
    async fn reclassify_imported_settlement(
        &self,
        command: ReclassifyImportedSettlement,
    ) -> Result<InternalAccountingResult, LedgerError>;
    async fn write_off_liability_or_receivable(
        &self,
        command: WriteOffLiabilityOrReceivable,
    ) -> Result<InternalAccountingResult, LedgerError>;
    async fn record_cash_control_settlement(
        &self,
        command: RecordCashControlSettlement,
    ) -> Result<InternalAccountingResult, LedgerError>;
    async fn cancel_or_reverse_cash_control_settlement(
        &self,
        command: CancelOrReverseCashControlSettlement,
    ) -> Result<InternalAccountingResult, LedgerError>;
}

#[async_trait]
impl<U, Q, P> LedgerCommandCapability for LedgerApplication<U, Q, P>
where
    U: LedgerUnitOfWork + Send + Sync,
    Q: LedgerQueryPort + Send + Sync,
    P: Send + Sync,
{
    async fn open_account(&self, command: OpenAccount) -> Result<AccountResult, LedgerError> {
        LedgerApplication::open_account(self, command).await
    }
    async fn open_provider_observed_account(
        &self,
        command: OpenProviderObservedAccount,
    ) -> Result<AccountResult, LedgerError> {
        LedgerApplication::open_provider_observed_account(self, command).await
    }
    async fn rename_account(&self, command: RenameAccount) -> Result<AccountResult, LedgerError> {
        LedgerApplication::rename_account(self, command).await
    }
    async fn archive_account(&self, command: ArchiveAccount) -> Result<AccountResult, LedgerError> {
        LedgerApplication::archive_account(self, command).await
    }
    async fn restore_account(&self, command: RestoreAccount) -> Result<AccountResult, LedgerError> {
        LedgerApplication::restore_account(self, command).await
    }
    async fn record_manual_transaction(
        &self,
        command: RecordManualTransaction,
    ) -> Result<TransactionResult, LedgerError> {
        LedgerApplication::record_manual_transaction(self, command).await
    }
    async fn transfer(&self, command: TransferFunds) -> Result<TransferResult, LedgerError> {
        LedgerApplication::transfer(self, command).await
    }
    async fn update_annotation(
        &self,
        command: UpdateTransactionAnnotation,
    ) -> Result<AnnotationResult, LedgerError> {
        LedgerApplication::update_annotation(self, command).await
    }
    async fn apply_category_assignment(
        &self,
        command: ApplyCategoryAssignment,
    ) -> Result<CategoryAssignmentResult, LedgerError> {
        LedgerApplication::apply_category_assignment(self, command).await
    }
    async fn restore_category_assignment(
        &self,
        command: RestoreCategoryAssignment,
    ) -> Result<CategoryAssignmentResult, LedgerError> {
        LedgerApplication::restore_category_assignment(self, command).await
    }
    async fn enable_automatic_classification(
        &self,
        command: EnableAutomaticClassification,
    ) -> Result<CategoryAssignmentResult, LedgerError> {
        LedgerApplication::enable_automatic_classification(self, command).await
    }
    async fn correct_balance(
        &self,
        command: CorrectBalance,
    ) -> Result<FinancialChangeResult, LedgerError> {
        LedgerApplication::correct_balance(self, command).await
    }
    async fn reverse_transaction(
        &self,
        command: ReverseTransaction,
    ) -> Result<FinancialChangeResult, LedgerError> {
        LedgerApplication::reverse_transaction(self, command).await
    }
    async fn replace_transaction(
        &self,
        command: ReplaceTransaction,
    ) -> Result<ReplacementResult, LedgerError> {
        LedgerApplication::replace_transaction(self, command).await
    }
}

#[async_trait]
impl<U, Q, P> LedgerQueryCapability for LedgerApplication<U, Q, P>
where
    U: Send + Sync,
    Q: LedgerQueryPort + Send + Sync,
    P: ProjectionRebuildPort + Send + Sync,
{
    async fn validate_provider_account_binding(
        &self,
        command: ValidateProviderAccountBinding,
    ) -> Result<ProviderAccountBindingResult, LedgerError> {
        LedgerApplication::validate_provider_account_binding(self, command).await
    }
    async fn list_accounts(&self, user_id: UserId) -> Result<Vec<AccountView>, LedgerError> {
        LedgerApplication::list_accounts(self, user_id).await
    }
    async fn get_account(
        &self,
        user_id: UserId,
        id: LedgerAccountId,
    ) -> Result<AccountView, LedgerError> {
        LedgerApplication::get_account(self, user_id, id).await
    }
    async fn account_activity(
        &self,
        user_id: UserId,
        account_id: LedgerAccountId,
        after: Option<ActivityCursor>,
        limit: u32,
    ) -> Result<Vec<JournalView>, LedgerError> {
        LedgerApplication::account_activity(self, user_id, account_id, after, limit).await
    }
    async fn list_journals(
        &self,
        user_id: UserId,
        after: Option<ActivityCursor>,
        limit: u32,
    ) -> Result<Vec<JournalView>, LedgerError> {
        LedgerApplication::list_journals(self, user_id, after, limit).await
    }
    async fn list_activity(
        &self,
        user_id: UserId,
        filter: ActivityFilter,
        after: Option<ActivityCursor>,
        limit: u32,
    ) -> Result<Vec<JournalView>, LedgerError> {
        LedgerApplication::list_activity(self, user_id, filter, after, limit).await
    }
    async fn summarize_activity(
        &self,
        user_id: UserId,
        filter: ActivityFilter,
    ) -> Result<ActivitySummary, LedgerError> {
        LedgerApplication::summarize_activity(self, user_id, filter).await
    }
    async fn get_journal(
        &self,
        user_id: UserId,
        id: JournalEntryId,
    ) -> Result<JournalView, LedgerError> {
        LedgerApplication::get_journal(self, user_id, id).await
    }
    async fn verify_projection(&self) -> Result<Vec<ProjectionMismatch>, LedgerError> {
        LedgerApplication::verify_projection(self).await
    }
    async fn rebuild_projection(&self) -> Result<(), LedgerError> {
        LedgerApplication::rebuild_projection(self).await
    }
}

#[async_trait]
impl<U, Q, P> LedgerReconciliationCapability for LedgerApplication<U, Q, P>
where
    U: LedgerUnitOfWork + Send + Sync,
    Q: LedgerQueryPort + Send + Sync,
    P: Send + Sync,
{
    async fn observe_provider_balance(
        &self,
        command: ObserveProviderBalance,
    ) -> Result<ReconciliationResult, LedgerError> {
        LedgerApplication::observe_provider_balance(self, command).await
    }
    async fn approve_reconciliation(
        &self,
        command: ApproveReconciliation,
    ) -> Result<ReconciliationResult, LedgerError> {
        LedgerApplication::approve_reconciliation(self, command).await
    }
    async fn dismiss_reconciliation(
        &self,
        command: DismissReconciliation,
    ) -> Result<ReconciliationResult, LedgerError> {
        LedgerApplication::dismiss_reconciliation(self, command).await
    }
    async fn list_reconciliations(
        &self,
        user_id: UserId,
    ) -> Result<Vec<ReconciliationView>, LedgerError> {
        LedgerApplication::list_reconciliations(self, user_id).await
    }
    async fn get_reconciliation(
        &self,
        user_id: UserId,
        id: ReconciliationCaseId,
    ) -> Result<ReconciliationView, LedgerError> {
        LedgerApplication::get_reconciliation(self, user_id, id).await
    }
}

#[async_trait]
impl<U, Q, P> LedgerInternalAccountingCapability for LedgerApplication<U, Q, P>
where
    U: LedgerUnitOfWork + Send + Sync,
    Q: Send + Sync,
    P: Send + Sync,
{
    async fn ensure_typed_control_account(
        &self,
        command: EnsureTypedControlAccount,
    ) -> Result<ControlAccountResult, LedgerError> {
        LedgerApplication::ensure_typed_control_account(self, command).await
    }
    async fn record_expense_and_control_balances(
        &self,
        command: RecordExpenseAndControlBalances,
    ) -> Result<InternalAccountingResult, LedgerError> {
        LedgerApplication::record_expense_and_control_balances(self, command).await
    }
    async fn import_provider_transaction(
        &self,
        command: ImportProviderTransaction,
    ) -> Result<InternalAccountingResult, LedgerError> {
        LedgerApplication::import_provider_transaction(self, command).await
    }
    async fn transition_provider_transaction_state(
        &self,
        command: TransitionProviderTransactionState,
    ) -> Result<InternalAccountingResult, LedgerError> {
        LedgerApplication::transition_provider_transaction_state(self, command).await
    }
    async fn reverse_provider_transaction(
        &self,
        command: ReverseProviderTransaction,
    ) -> Result<InternalAccountingResult, LedgerError> {
        LedgerApplication::reverse_provider_transaction(self, command).await
    }
    async fn settle_receivable_or_payable(
        &self,
        command: SettleReceivableOrPayable,
    ) -> Result<InternalAccountingResult, LedgerError> {
        LedgerApplication::settle_receivable_or_payable(self, command).await
    }
    async fn record_principal_disbursement(
        &self,
        command: RecordPrincipalDisbursement,
    ) -> Result<InternalAccountingResult, LedgerError> {
        LedgerApplication::record_principal_disbursement(self, command).await
    }
    async fn record_principal_repayment(
        &self,
        command: RecordPrincipalRepayment,
    ) -> Result<InternalAccountingResult, LedgerError> {
        LedgerApplication::record_principal_repayment(self, command).await
    }
    async fn record_interest_and_fee(
        &self,
        command: RecordInterestAndFee,
    ) -> Result<InternalAccountingResult, LedgerError> {
        LedgerApplication::record_interest_and_fee(self, command).await
    }
    async fn record_interest_or_fee_accrual(
        &self,
        command: RecordInterestOrFeeAccrual,
    ) -> Result<InternalAccountingResult, LedgerError> {
        LedgerApplication::record_interest_or_fee_accrual(self, command).await
    }
    async fn reclassify_expense_to_receivable_or_payable(
        &self,
        command: ReclassifyExpenseToReceivableOrPayable,
    ) -> Result<InternalAccountingResult, LedgerError> {
        LedgerApplication::reclassify_expense_to_receivable_or_payable(self, command).await
    }
    async fn reclassify_imported_settlement(
        &self,
        command: ReclassifyImportedSettlement,
    ) -> Result<InternalAccountingResult, LedgerError> {
        LedgerApplication::reclassify_imported_settlement(self, command).await
    }
    async fn write_off_liability_or_receivable(
        &self,
        command: WriteOffLiabilityOrReceivable,
    ) -> Result<InternalAccountingResult, LedgerError> {
        LedgerApplication::write_off_liability_or_receivable(self, command).await
    }
    async fn record_cash_control_settlement(
        &self,
        command: RecordCashControlSettlement,
    ) -> Result<InternalAccountingResult, LedgerError> {
        LedgerApplication::record_cash_control_settlement(self, command).await
    }
    async fn cancel_or_reverse_cash_control_settlement(
        &self,
        command: CancelOrReverseCashControlSettlement,
    ) -> Result<InternalAccountingResult, LedgerError> {
        LedgerApplication::cancel_or_reverse_cash_control_settlement(self, command).await
    }
}
