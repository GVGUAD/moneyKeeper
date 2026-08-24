//! Durable Sharing process runner. Ledger calls are idempotent and Sharing
//! finalization is fenced by the claimed process lease.

use chrono::{Duration, Utc};

use crate::{
    contexts::{
        ledger::public::{JournalEntryId, LedgerFacade},
        sharing::public::{
            CompleteBillAccounting, CompleteBillCancellation, CompleteSettlementAccounting,
            CompleteSettlementReversal, FailBillAccounting, FailSettlementAccounting,
            JournalReversal, RetrySharingWorkflow, SharingError, SharingFacade,
            SharingWorkflowWork, WorkflowClaim,
        },
    },
    integration::process_managers::{
        sharing_accounting::SharingAccountingCoordinator,
        sharing_settlement::SharingSettlementCoordinator,
    },
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SharingWorkflowReport {
    pub claimed: bool,
    pub posted: bool,
    pub retry_due: bool,
}

#[derive(Clone)]
pub struct SharingWorkflowWorker {
    sharing: SharingFacade,
    accounting: SharingAccountingCoordinator,
    settlements: SharingSettlementCoordinator,
    holder: String,
}

impl SharingWorkflowWorker {
    pub fn new(sharing: SharingFacade, ledger: LedgerFacade) -> Self {
        Self {
            sharing,
            accounting: SharingAccountingCoordinator::new(ledger.clone()),
            settlements: SharingSettlementCoordinator::new(ledger),
            holder: format!("sharing-workflow:{}", uuid::Uuid::new_v4()),
        }
    }

    pub async fn run_once(&self) -> anyhow::Result<SharingWorkflowReport> {
        let Some(work) = self.sharing.claim_next_work(&self.holder).await? else {
            return Ok(SharingWorkflowReport::default());
        };
        match work {
            SharingWorkflowWork::BillAccounting {
                claim,
                bill,
                journals_to_reverse,
            } => {
                if let Err(error) = self.accounting.preflight_manual_revision(&bill).await {
                    return self.handle_bill_error(claim, &bill, vec![], error).await;
                }
                let mut reversed_journals = Vec::with_capacity(journals_to_reverse.len());
                for journal_id in journals_to_reverse {
                    match self
                        .accounting
                        .reverse_revision(
                            &bill,
                            JournalEntryId::new(journal_id),
                            "Sharing bill revision replaced".into(),
                        )
                        .await
                    {
                        Ok(result) => reversed_journals.push(JournalReversal {
                            original_journal_id: journal_id,
                            reversal_journal_id: result.journal_entry_id.into_uuid(),
                        }),
                        Err(error) => return self.retry(claim, error.to_string()).await,
                    }
                }
                let outcome = match self.accounting.account_manual_revision(&bill).await {
                    Ok(value) => value,
                    Err(error) => {
                        return self
                            .handle_bill_error(claim, &bill, reversed_journals.clone(), error)
                            .await;
                    }
                };
                if let Err(error) = self
                    .sharing
                    .complete_bill_accounting(CompleteBillAccounting {
                        claim: Some(claim.clone()),
                        user_id: bill.user_id(),
                        bill_id: bill.id(),
                        revision: bill.current_revision().number,
                        expected_version: bill.version(),
                        journal_ids: outcome
                            .journal_ids
                            .into_iter()
                            .map(JournalEntryId::into_uuid)
                            .collect(),
                        reversed_journals,
                        correlation_id: claim.correlation_id,
                        occurred_at: Utc::now(),
                    })
                    .await
                {
                    return self.finalization_error(claim, error).await;
                }
                Ok(posted())
            }
            SharingWorkflowWork::BillCancellation {
                claim,
                bill,
                journals_to_reverse,
            } => {
                let mut reversed_journals = Vec::with_capacity(journals_to_reverse.len());
                for journal_id in journals_to_reverse {
                    match self
                        .accounting
                        .reverse_revision(
                            &bill,
                            JournalEntryId::new(journal_id),
                            "Sharing bill cancelled".into(),
                        )
                        .await
                    {
                        Ok(result) => reversed_journals.push(JournalReversal {
                            original_journal_id: journal_id,
                            reversal_journal_id: result.journal_entry_id.into_uuid(),
                        }),
                        Err(error) => return self.retry(claim, error.to_string()).await,
                    }
                }
                if let Err(error) = self
                    .sharing
                    .complete_bill_cancellation(CompleteBillCancellation {
                        claim: Some(claim.clone()),
                        user_id: bill.user_id(),
                        bill_id: bill.id(),
                        expected_version: bill.version(),
                        reversed_journals,
                        correlation_id: claim.correlation_id,
                        occurred_at: Utc::now(),
                    })
                    .await
                {
                    return self.finalization_error(claim, error).await;
                }
                Ok(posted())
            }
            SharingWorkflowWork::SettlementAccounting { claim, settlement } => {
                let result = match self
                    .settlements
                    .post(&settlement, claim.correlation_id)
                    .await
                {
                    Ok(value) => value,
                    Err(error) => {
                        if matches!(error, SharingError::Persistence(_)) {
                            return self.retry(claim, error.to_string()).await;
                        }
                        if let Err(finalization_error) = self
                            .sharing
                            .fail_settlement_accounting(FailSettlementAccounting {
                                claim: claim.clone(),
                                user_id: settlement.user_id(),
                                bill_id: settlement.bill_id(),
                                settlement_id: settlement.id(),
                                expected_version: settlement.version(),
                                error: error.to_string(),
                                occurred_at: Utc::now(),
                            })
                            .await
                        {
                            return self.finalization_error(claim, finalization_error).await;
                        }
                        return Ok(failed());
                    }
                };
                if let Err(error) = self
                    .sharing
                    .complete_settlement_accounting(CompleteSettlementAccounting {
                        claim: Some(claim.clone()),
                        user_id: settlement.user_id(),
                        bill_id: settlement.bill_id(),
                        settlement_id: settlement.id(),
                        expected_version: settlement.version(),
                        journal_id: result.journal_entry_id.map(JournalEntryId::into_uuid),
                        correlation_id: claim.correlation_id,
                        occurred_at: Utc::now(),
                    })
                    .await
                {
                    return self.finalization_error(claim, error).await;
                }
                Ok(posted())
            }
            SharingWorkflowWork::SettlementReversal {
                claim,
                settlement,
                accounting_journal_id,
                reason,
            } => {
                let reversal_journal_id = if let Some(journal_id) = accounting_journal_id {
                    match self
                        .settlements
                        .reverse_posted(
                            &settlement,
                            JournalEntryId::new(journal_id),
                            claim.correlation_id,
                            reason,
                        )
                        .await
                    {
                        Ok(result) => Some(result.journal_entry_id.into_uuid()),
                        Err(error) => return self.retry(claim, error.to_string()).await,
                    }
                } else {
                    None
                };
                if let Err(error) = self
                    .sharing
                    .complete_settlement_reversal(CompleteSettlementReversal {
                        claim: Some(claim.clone()),
                        user_id: settlement.user_id(),
                        bill_id: settlement.bill_id(),
                        settlement_id: settlement.id(),
                        reversal_journal_id,
                        correlation_id: claim.correlation_id,
                        occurred_at: Utc::now(),
                    })
                    .await
                {
                    return self.finalization_error(claim, error).await;
                }
                Ok(posted())
            }
        }
    }

    async fn handle_bill_error(
        &self,
        claim: WorkflowClaim,
        bill: &crate::contexts::sharing::public::BillSplit,
        reversed_journals: Vec<JournalReversal>,
        error: SharingError,
    ) -> anyhow::Result<SharingWorkflowReport> {
        if matches!(error, SharingError::Persistence(_)) {
            return self.retry(claim, error.to_string()).await;
        }
        if let Err(finalization_error) = self
            .sharing
            .fail_bill_accounting(FailBillAccounting {
                claim: claim.clone(),
                user_id: bill.user_id(),
                bill_id: bill.id(),
                revision: bill.current_revision().number,
                expected_version: bill.version(),
                reversed_journals,
                error: error.to_string(),
                occurred_at: Utc::now(),
            })
            .await
        {
            return self.finalization_error(claim, finalization_error).await;
        }
        Ok(failed())
    }

    async fn finalization_error(
        &self,
        claim: WorkflowClaim,
        error: SharingError,
    ) -> anyhow::Result<SharingWorkflowReport> {
        if matches!(error, SharingError::Persistence(_)) {
            self.retry(claim, error.to_string()).await
        } else {
            Err(error.into())
        }
    }

    async fn retry(
        &self,
        claim: WorkflowClaim,
        error: String,
    ) -> anyhow::Result<SharingWorkflowReport> {
        let exponent = claim.attempt.saturating_sub(1).min(9);
        let seconds = 1_i64.checked_shl(exponent).unwrap_or(300).min(300);
        self.sharing
            .retry_work(RetrySharingWorkflow {
                claim,
                error,
                retry_at: Utc::now() + Duration::seconds(seconds),
            })
            .await?;
        Ok(SharingWorkflowReport {
            claimed: true,
            posted: false,
            retry_due: true,
        })
    }
}

fn posted() -> SharingWorkflowReport {
    SharingWorkflowReport {
        claimed: true,
        posted: true,
        retry_due: false,
    }
}

fn failed() -> SharingWorkflowReport {
    SharingWorkflowReport {
        claimed: true,
        posted: false,
        retry_due: false,
    }
}
