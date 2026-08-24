//! Sharing-to-Ledger accounting coordinator using closed Ledger recipes.

use crate::{
    contexts::{ledger::public::*, sharing::public::*},
    shared_kernel::{CausationId, CorrelationId, IdempotencyKey, Money, UserId},
};
use rust_decimal::Decimal;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BillAccountingState {
    PendingAccounting,
    Posted,
    RetryDue,
    Failed,
    PendingCancellation,
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BillAccountingProcess {
    pub bill_id: BillSplitId,
    pub revision: u32,
    pub state: BillAccountingState,
    pub correlation_id: CorrelationId,
    pub journal_id: Option<JournalEntryId>,
    pub reversal_journal_id: Option<JournalEntryId>,
    pub last_error: Option<String>,
}

impl BillAccountingProcess {
    pub fn start(bill_id: BillSplitId, revision: u32, correlation_id: CorrelationId) -> Self {
        Self {
            bill_id,
            revision,
            state: BillAccountingState::PendingAccounting,
            correlation_id,
            journal_id: None,
            reversal_journal_id: None,
            last_error: None,
        }
    }
    pub fn accounting_key(
        &self,
    ) -> Result<IdempotencyKey, crate::shared_kernel::IdempotencyKeyError> {
        IdempotencyKey::new(format!(
            "sharing-bill-accounting:{}:{}",
            self.bill_id, self.revision
        ))
    }
    pub fn reversal_key(
        &self,
    ) -> Result<IdempotencyKey, crate::shared_kernel::IdempotencyKeyError> {
        IdempotencyKey::new(format!(
            "sharing-bill-accounting-reversal:{}:{}",
            self.bill_id, self.revision
        ))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountingRecipe {
    pub contribution: Decimal,
    pub share: Decimal,
    pub receivable: Decimal,
    pub payable: Decimal,
}

impl AccountingRecipe {
    pub fn from_revision(revision: &BillRevision) -> Result<Self, SharingError> {
        let contribution = revision
            .contributions
            .iter()
            .filter(|value| value.participant == Participant::CurrentUser)
            .map(|value| value.amount.amount())
            .sum();
        let share = revision
            .shares
            .iter()
            .find(|value| value.participant == Participant::CurrentUser)
            .map_or(Decimal::ZERO, |value| value.amount.amount());
        let receivable = revision
            .obligations
            .iter()
            .filter(|value| value.creditor == Participant::CurrentUser)
            .map(|value| value.amount.amount())
            .sum();
        let payable = revision
            .obligations
            .iter()
            .filter(|value| value.debtor == Participant::CurrentUser)
            .map(|value| value.amount.amount())
            .sum();
        if receivable - payable != contribution - share {
            return Err(SharingError::ArithmeticOverflow);
        }
        Ok(Self {
            contribution,
            share,
            receivable,
            payable,
        })
    }
}

#[derive(Clone)]
pub struct SharingAccountingCoordinator {
    ledger: LedgerFacade,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BillAccountingOutcome {
    pub journal_ids: Vec<JournalEntryId>,
    pub replayed: bool,
}

impl SharingAccountingCoordinator {
    pub fn new(ledger: LedgerFacade) -> Self {
        Self { ledger }
    }

    pub async fn preflight_manual_revision(&self, bill: &BillSplit) -> Result<(), SharingError> {
        let revision = bill.current_revision();
        AccountingRecipe::from_revision(revision)?;
        for contribution in &revision.contributions {
            if contribution.participant != Participant::CurrentUser {
                continue;
            }
            match &contribution.evidence {
                ContributionEvidence::Manual { account_id } => {
                    let account = self
                        .ledger
                        .get_account(bill.user_id(), LedgerAccountId::new(account_id.into_uuid()))
                        .await
                        .map_err(ledger_error)?;
                    if account.currency != *revision.total.currency() {
                        return Err(SharingError::BillAccountingValidation(
                            "manual contribution account currency does not match the bill".into(),
                        ));
                    }
                    if account.lifecycle == AccountLifecycle::Archived {
                        return Err(SharingError::BillAccountingValidation(
                            "manual contribution account is archived".into(),
                        ));
                    }
                    if account.authority == AccountAuthority::System {
                        return Err(SharingError::BillAccountingValidation(
                            "manual contribution account must be user-managed".into(),
                        ));
                    }
                }
                ContributionEvidence::ExistingJournals { allocations } => {
                    for value in allocations {
                        let journal = self
                            .ledger
                            .get_journal(
                                bill.user_id(),
                                JournalEntryId::new(value.journal_id.into_uuid()),
                            )
                            .await
                            .map_err(ledger_error)?;
                        let eligible: Decimal = journal
                            .postings
                            .iter()
                            .filter(|posting| {
                                posting.currency == *revision.total.currency()
                                    && posting.account_nature == AccountNature::Expense
                                    && posting.signed_amount.is_sign_positive()
                            })
                            .map(|posting| posting.signed_amount)
                            .sum();
                        if !matches!(
                            journal.source,
                            JournalSource::Import | JournalSource::Manual
                        ) {
                            return Err(SharingError::BillAccountingValidation(
                                "selected journal is not an imported or manual transaction".into(),
                            ));
                        }
                        if journal.purpose != PostingPurpose::Ordinary {
                            return Err(SharingError::BillAccountingValidation(
                                "selected journal is not an ordinary transaction".into(),
                            ));
                        }
                        if journal.reversed_by_journal_id.is_some() {
                            return Err(SharingError::BillAccountingValidation(
                                "selected journal has been reversed".into(),
                            ));
                        }
                        if journal.replaced_by_journal_id.is_some() {
                            return Err(SharingError::BillAccountingValidation(
                                "selected journal has been replaced".into(),
                            ));
                        }
                        if eligible.is_zero() {
                            return Err(SharingError::BillAccountingValidation(
                                "selected journal is not an outgoing expense in the bill currency"
                                    .into(),
                            ));
                        }
                        if eligible < value.amount.amount() {
                            return Err(SharingError::BillAccountingValidation(
                                "selected journal allocation exceeds its expense amount".into(),
                            ));
                        }
                    }
                }
                ContributionEvidence::External => {
                    return Err(SharingError::BillAccountingValidation(
                        "current-user contributions require manual-account or journal evidence"
                            .into(),
                    ));
                }
            }
        }
        Ok(())
    }

    pub async fn account_manual_revision(
        &self,
        bill: &BillSplit,
    ) -> Result<BillAccountingOutcome, SharingError> {
        let revision = bill.current_revision();
        self.preflight_manual_revision(bill).await?;
        let mut cash = Vec::new();
        let mut existing_journals = Vec::new();
        for contribution in &revision.contributions {
            if contribution.participant != Participant::CurrentUser {
                continue;
            }
            match &contribution.evidence {
                ContributionEvidence::Manual { account_id } => {
                    let account_id = LedgerAccountId::new(account_id.into_uuid());
                    cash.push(CashContribution {
                        account_id,
                        amount: contribution.amount.clone(),
                    });
                }
                ContributionEvidence::ExistingJournals { allocations } => {
                    for value in allocations {
                        let journal_id = JournalEntryId::new(value.journal_id.into_uuid());
                        existing_journals.push((journal_id, value.amount.clone()));
                    }
                }
                ContributionEvidence::External => return Err(SharingError::InvalidContribution),
            }
        }
        let mut receivables = Vec::new();
        let mut payables = Vec::new();
        for obligation in &revision.obligations {
            let (contact, role, target) = if obligation.creditor == Participant::CurrentUser {
                match obligation.debtor {
                    Participant::Contact(id) => {
                        (id, ControlAccountRole::ExternalReceivable, &mut receivables)
                    }
                    Participant::CurrentUser => continue,
                }
            } else if obligation.debtor == Participant::CurrentUser {
                match obligation.creditor {
                    Participant::Contact(id) => {
                        (id, ControlAccountRole::ExternalPayable, &mut payables)
                    }
                    Participant::CurrentUser => continue,
                }
            } else {
                continue;
            };
            let control = self
                .ledger
                .ensure_typed_control_account(EnsureTypedControlAccount {
                    metadata: ledger_metadata(
                        bill.user_id(),
                        bill.id(),
                        revision.number,
                        revision.accounting_correlation_id,
                        revision.occurred_at,
                        &format!("control:{contact}:{role:?}"),
                    )?,
                    role,
                    subject_reference: format!("contact:{contact}"),
                    currency: revision.total.currency().clone(),
                })
                .await
                .map_err(ledger_error)?;
            target.push(ControlAmount {
                account_id: control.account_id,
                amount: obligation.amount.clone(),
            });
        }
        let mut outcome = BillAccountingOutcome::default();
        let mut receivable_index = 0usize;
        let mut receivable_remaining: Vec<Decimal> = receivables
            .iter()
            .map(|value| value.amount.amount())
            .collect();
        for (source_index, (source_journal_id, source_amount)) in
            existing_journals.iter().enumerate()
        {
            let mut source_remaining = source_amount.amount();
            while source_remaining > Decimal::ZERO && receivable_index < receivables.len() {
                if receivable_remaining[receivable_index].is_zero() {
                    receivable_index += 1;
                    continue;
                }
                let chunk = source_remaining.min(receivable_remaining[receivable_index]);
                let result = self
                    .ledger
                    .reclassify_expense_to_receivable_or_payable(
                        ReclassifyExpenseToReceivableOrPayable {
                            metadata: ledger_metadata(
                                bill.user_id(),
                                bill.id(),
                                revision.number,
                                revision.accounting_correlation_id,
                                revision.occurred_at,
                                &format!("reclassify:{source_index}:{receivable_index}"),
                            )?,
                            original_expense_journal_id: *source_journal_id,
                            control_account_id: receivables[receivable_index].account_id,
                            amount: Money::new(
                                chunk,
                                revision.total.currency().clone(),
                                revision.total.amount().scale(),
                            )?,
                            direction: ControlDirection::Receivable,
                        },
                    )
                    .await
                    .map_err(ledger_error)?;
                if let Some(journal_id) = result.journal_entry_id {
                    outcome.journal_ids.push(journal_id);
                }
                outcome.replayed |= result.replayed;
                source_remaining -= chunk;
                receivable_remaining[receivable_index] -= chunk;
            }
        }
        let remaining_receivables = receivables
            .into_iter()
            .zip(receivable_remaining)
            .filter(|(_, amount)| *amount > Decimal::ZERO)
            .map(|(control, amount)| {
                Ok(ControlAmount {
                    account_id: control.account_id,
                    amount: Money::new(
                        amount,
                        revision.total.currency().clone(),
                        revision.total.amount().scale(),
                    )?,
                })
            })
            .collect::<Result<Vec<_>, SharingError>>()?;
        let cash_total: Decimal = cash.iter().map(|value| value.amount.amount()).sum();
        let receivable_total: Decimal = remaining_receivables
            .iter()
            .map(|value| value.amount.amount())
            .sum();
        let payable_total: Decimal = payables.iter().map(|value| value.amount.amount()).sum();
        let expense = cash_total
            .checked_sub(receivable_total)
            .and_then(|value| value.checked_add(payable_total))
            .ok_or(SharingError::ArithmeticOverflow)?;
        if !cash.is_empty()
            || !remaining_receivables.is_empty()
            || !payables.is_empty()
            || expense > Decimal::ZERO
        {
            let result = self
                .ledger
                .record_expense_and_control_balances(RecordExpenseAndControlBalances {
                    metadata: ledger_metadata(
                        bill.user_id(),
                        bill.id(),
                        revision.number,
                        revision.accounting_correlation_id,
                        revision.occurred_at,
                        "manual-and-payable",
                    )?,
                    cash_contributions: cash,
                    expense: Money::new(
                        expense,
                        revision.total.currency().clone(),
                        revision.total.amount().scale(),
                    )?,
                    receivables: remaining_receivables,
                    payables,
                    description: revision.title.clone(),
                })
                .await
                .map_err(ledger_error)?;
            if let Some(journal_id) = result.journal_entry_id {
                outcome.journal_ids.push(journal_id);
            }
            outcome.replayed |= result.replayed;
        }
        Ok(outcome)
    }

    pub async fn reverse_revision(
        &self,
        bill: &BillSplit,
        journal_id: JournalEntryId,
        reason: String,
    ) -> Result<FinancialChangeResult, SharingError> {
        let revision = bill.current_revision();
        self.ledger
            .reverse_transaction(ReverseTransaction {
                user_id: bill.user_id(),
                journal_entry_id: journal_id,
                reason,
                idempotency_key: IdempotencyKey::new(format!(
                    "sharing-bill-accounting-reversal:{}:{}:{}",
                    bill.id(),
                    revision.number,
                    journal_id,
                ))
                .map_err(|error| SharingError::Persistence(error.to_string()))?,
                correlation_id: revision.accounting_correlation_id,
                causation_id: None,
                occurred_at: revision.occurred_at,
            })
            .await
            .map_err(ledger_error)
    }
}

fn ledger_metadata(
    user: UserId,
    bill: BillSplitId,
    revision: u32,
    correlation: CorrelationId,
    occurred_at: chrono::DateTime<chrono::Utc>,
    action: &str,
) -> Result<InternalCommandMetadata, SharingError> {
    Ok(InternalCommandMetadata {
        user_id: user,
        source: SourceReference::new(
            "sharing",
            format!("bill:{bill}"),
            format!("revision:{revision}:{action}"),
        )
        .map_err(ledger_error)?,
        correlation_id: correlation,
        causation_id: None::<CausationId>,
        idempotency_key: IdempotencyKey::new(format!(
            "sharing-bill-accounting:{bill}:{revision}:{action}"
        ))
        .map_err(|error| SharingError::Persistence(error.to_string()))?,
        occurred_at,
    })
}
fn ledger_error(error: LedgerError) -> SharingError {
    if error.is_persistence() || error.is_version_conflict() {
        SharingError::Persistence(error.to_string())
    } else {
        SharingError::BillAccountingValidation(error.to_string())
    }
}
