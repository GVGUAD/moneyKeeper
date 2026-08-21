//! Sharing settlement and reversal coordinator.

use crate::{
    contexts::{ledger::public::*, sharing::public::*},
    shared_kernel::{CorrelationId, IdempotencyKey},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettlementAccountingState {
    PendingAccounting,
    Posted,
    RetryDue,
    Failed,
    PendingReversal,
    Reversed,
    TerminalNoEffect,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SettlementAccountingProcess {
    pub settlement_id: SettlementId,
    pub state: SettlementAccountingState,
    pub correlation_id: CorrelationId,
    pub journal_id: Option<JournalEntryId>,
    pub reversal_journal_id: Option<JournalEntryId>,
}
impl SettlementAccountingProcess {
    pub fn start(id: SettlementId, correlation_id: CorrelationId) -> Self {
        Self {
            settlement_id: id,
            state: SettlementAccountingState::PendingAccounting,
            correlation_id,
            journal_id: None,
            reversal_journal_id: None,
        }
    }
    pub fn posting_key(&self) -> Result<IdempotencyKey, crate::shared_kernel::IdempotencyKeyError> {
        IdempotencyKey::new(format!("sharing-settlement:{}", self.settlement_id))
    }
    pub fn reversal_key(
        &self,
    ) -> Result<IdempotencyKey, crate::shared_kernel::IdempotencyKeyError> {
        IdempotencyKey::new(format!(
            "sharing-settlement-reversal:{}",
            self.settlement_id
        ))
    }
}

#[derive(Clone)]
pub struct SharingSettlementCoordinator {
    ledger: LedgerFacade,
}
impl SharingSettlementCoordinator {
    pub fn new(ledger: LedgerFacade) -> Self {
        Self { ledger }
    }
    pub async fn post_manual(
        &self,
        settlement: &Settlement,
        cash_account: LedgerAccountId,
        control_account: LedgerAccountId,
        correlation: CorrelationId,
    ) -> Result<InternalAccountingResult, SharingError> {
        if settlement.creditor() != Participant::CurrentUser
            && settlement.debtor() != Participant::CurrentUser
        {
            return Ok(InternalAccountingResult {
                journal_entry_id: None,
                effects: vec![],
                projection_versions: vec![],
                replayed: false,
                cancelled: false,
                outbox_correlation_id: correlation,
            });
        }
        let metadata = InternalCommandMetadata {
            user_id: settlement.user_id(),
            source: SourceReference::new(
                "sharing",
                format!("bill:{}", settlement.bill_id()),
                format!("settlement:{}", settlement.id()),
            )
            .map_err(map_ledger)?,
            correlation_id: correlation,
            causation_id: None,
            idempotency_key: IdempotencyKey::new(format!("sharing-settlement:{}", settlement.id()))
                .map_err(|error| SharingError::Persistence(error.to_string()))?,
            occurred_at: settlement.occurred_at(),
        };
        self.ledger
            .record_cash_control_settlement(RecordCashControlSettlement {
                metadata,
                cash_account_id: cash_account,
                control_account_id: control_account,
                amount: settlement.amount().clone(),
                source_operation_id: format!("sharing-settlement:{}", settlement.id()),
            })
            .await
            .map_err(map_ledger)
    }

    pub async fn post_imported(
        &self,
        settlement: &Settlement,
        control_account: LedgerAccountId,
        imported_journal_id: JournalEntryId,
        correlation: CorrelationId,
    ) -> Result<InternalAccountingResult, SharingError> {
        let direction = if settlement.creditor() == Participant::CurrentUser {
            ControlDirection::Receivable
        } else if settlement.debtor() == Participant::CurrentUser {
            ControlDirection::Payable
        } else {
            return Ok(InternalAccountingResult {
                journal_entry_id: None,
                effects: vec![],
                projection_versions: vec![],
                replayed: false,
                cancelled: false,
                outbox_correlation_id: correlation,
            });
        };
        let metadata = InternalCommandMetadata {
            user_id: settlement.user_id(),
            source: SourceReference::new(
                "sharing",
                format!("bill:{}", settlement.bill_id()),
                format!("settlement-import:{}", settlement.id()),
            )
            .map_err(map_ledger)?,
            correlation_id: correlation,
            causation_id: None,
            idempotency_key: IdempotencyKey::new(format!("sharing-settlement:{}", settlement.id()))
                .map_err(|error| SharingError::Persistence(error.to_string()))?,
            occurred_at: settlement.occurred_at(),
        };
        self.ledger
            .reclassify_imported_settlement(ReclassifyImportedSettlement {
                metadata,
                imported_journal_entry_id: imported_journal_id,
                control_account_id: control_account,
                amount: settlement.amount().clone(),
                direction,
            })
            .await
            .map_err(map_ledger)
    }

    pub async fn reverse_posted(
        &self,
        settlement: &Settlement,
        posted_journal_id: JournalEntryId,
        correlation: CorrelationId,
        reason: String,
    ) -> Result<FinancialChangeResult, SharingError> {
        self.ledger
            .reverse_transaction(ReverseTransaction {
                user_id: settlement.user_id(),
                journal_entry_id: posted_journal_id,
                reason,
                idempotency_key: IdempotencyKey::new(format!(
                    "sharing-settlement-reversal:{}",
                    settlement.id()
                ))
                .map_err(|error| SharingError::Persistence(error.to_string()))?,
                correlation_id: correlation,
                causation_id: None,
                occurred_at: chrono::Utc::now(),
            })
            .await
            .map_err(map_ledger)
    }
    pub async fn reverse(
        &self,
        settlement: &Settlement,
        correlation: CorrelationId,
        reason: String,
    ) -> Result<InternalAccountingResult, SharingError> {
        let metadata = InternalCommandMetadata {
            user_id: settlement.user_id(),
            source: SourceReference::new(
                "sharing",
                format!("bill:{}", settlement.bill_id()),
                format!("settlement-reversal:{}", settlement.id()),
            )
            .map_err(map_ledger)?,
            correlation_id: correlation,
            causation_id: None,
            idempotency_key: IdempotencyKey::new(format!(
                "sharing-settlement-reversal:{}",
                settlement.id()
            ))
            .map_err(|error| SharingError::Persistence(error.to_string()))?,
            occurred_at: chrono::Utc::now(),
        };
        self.ledger
            .cancel_or_reverse_cash_control_settlement(CancelOrReverseCashControlSettlement {
                metadata,
                source_operation_id: format!("sharing-settlement:{}", settlement.id()),
                reason,
            })
            .await
            .map_err(map_ledger)
    }
}
fn map_ledger(error: LedgerError) -> SharingError {
    SharingError::Persistence(error.to_string())
}
