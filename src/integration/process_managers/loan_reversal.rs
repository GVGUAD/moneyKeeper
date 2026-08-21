//! Loan movement reversal coordinator.

use crate::contexts::ledger::public::{LedgerFacade, ReverseTransaction};
use crate::contexts::loans::public::LoansFacade;
use crate::shared_kernel::IdempotencyKey;
use chrono::Utc;

#[derive(Clone)]
pub struct LoanReversalWorker {
    loans: LoansFacade,
    ledger: LedgerFacade,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LoanReversalReport {
    pub claimed: bool,
    pub posted: bool,
    pub retry_due: bool,
}
impl LoanReversalWorker {
    pub fn new(loans: LoansFacade, ledger: LedgerFacade) -> Self {
        Self { loans, ledger }
    }
    pub async fn run_once(&self) -> anyhow::Result<LoanReversalReport> {
        let Some(p) = self.loans.pending_reversals(1).await?.into_iter().next() else {
            return Ok(LoanReversalReport::default());
        };
        let Some(original) = p.pending.movement.ledger_journal_id else {
            return Err(anyhow::anyhow!(
                "posted loan movement has no Ledger journal"
            ));
        };
        let result = self
            .ledger
            .reverse_transaction(ReverseTransaction {
                user_id: p.pending.user_id,
                journal_entry_id: original,
                reason: p.reason.clone(),
                idempotency_key: IdempotencyKey::new(idempotency_key(p.pending.movement.id))?,
                correlation_id: p.pending.movement.correlation_id,
                causation_id: None,
                occurred_at: p.pending.movement.requested_at,
            })
            .await;
        match result {
            Ok(result) => {
                self.loans
                    .confirm_reversal(&p, result.journal_entry_id, Utc::now())
                    .await?;
                Ok(LoanReversalReport {
                    claimed: true,
                    posted: true,
                    retry_due: false,
                })
            }
            Err(_) => Ok(LoanReversalReport {
                claimed: true,
                posted: false,
                retry_due: true,
            }),
        }
    }
}
pub fn idempotency_key(movement_id: crate::contexts::loans::public::LoanMovementId) -> String {
    format!("loan-reversal:{movement_id}")
}
