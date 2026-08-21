//! Durable reverse-then-post replacement workflow state.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReplacementState {
    ReplacementRequested,
    ReversingOriginal,
    OriginalReversed,
    PostingReplacement,
    Posted,
    RetryDue,
    TerminalFailure,
    ReplacementFailedAfterReversal,
}
#[derive(Clone)]
pub struct LoanReplacementWorker {
    loans: crate::contexts::loans::public::LoansFacade,
    ledger: crate::contexts::ledger::public::LedgerFacade,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LoanReplacementReport {
    pub claimed: bool,
    pub original_reversed: bool,
    pub retry_due: bool,
}
impl LoanReplacementWorker {
    pub fn new(
        loans: crate::contexts::loans::public::LoansFacade,
        ledger: crate::contexts::ledger::public::LedgerFacade,
    ) -> Self {
        Self { loans, ledger }
    }
    pub async fn run_once(&self) -> anyhow::Result<LoanReplacementReport> {
        let Some(p) = self.loans.pending_replacements(1).await?.into_iter().next() else {
            return Ok(LoanReplacementReport::default());
        };
        let result = self
            .ledger
            .reverse_transaction(crate::contexts::ledger::public::ReverseTransaction {
                user_id: p.replacement.user_id,
                journal_entry_id: p.original_journal_id,
                reason: "Loan movement replacement".to_owned(),
                idempotency_key: crate::shared_kernel::IdempotencyKey::new(reversal_key(
                    p.original_movement_id,
                    p.replacement.movement.id,
                ))?,
                correlation_id: p.replacement.movement.correlation_id,
                causation_id: None,
                occurred_at: p.replacement.movement.requested_at,
            })
            .await;
        match result {
            Ok(result) => {
                self.loans
                    .confirm_replacement_reversal(&p, result.journal_entry_id, chrono::Utc::now())
                    .await?;
                Ok(LoanReplacementReport {
                    claimed: true,
                    original_reversed: true,
                    retry_due: false,
                })
            }
            Err(_) => Ok(LoanReplacementReport {
                claimed: true,
                original_reversed: false,
                retry_due: true,
            }),
        }
    }
}
pub fn reversal_key(
    original: crate::contexts::loans::public::LoanMovementId,
    replacement: crate::contexts::loans::public::LoanMovementId,
) -> String {
    format!("loan-replacement:{original}:{replacement}:reverse")
}
pub fn posting_key(
    original: crate::contexts::loans::public::LoanMovementId,
    replacement: crate::contexts::loans::public::LoanMovementId,
) -> String {
    format!("loan-replacement:{original}:{replacement}:post")
}
