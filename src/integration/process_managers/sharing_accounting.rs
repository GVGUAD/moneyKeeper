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

impl SharingAccountingCoordinator {
    pub fn new(ledger: LedgerFacade) -> Self {
        Self { ledger }
    }

    pub async fn account_manual_revision(
        &self,
        bill: &BillSplit,
    ) -> Result<InternalAccountingResult, SharingError> {
        let revision = bill.current_revision();
        let recipe = AccountingRecipe::from_revision(revision)?;
        let mut cash = Vec::new();
        let mut existing_journals = Vec::new();
        for contribution in &revision.contributions {
            if contribution.participant != Participant::CurrentUser {
                continue;
            }
            match &contribution.evidence {
                ContributionEvidence::Manual { account_id } => cash.push(CashContribution {
                    account_id: LedgerAccountId::new(account_id.into_uuid()),
                    amount: contribution.amount.clone(),
                }),
                ContributionEvidence::ExistingJournals { allocations } => existing_journals.extend(
                    allocations
                        .iter()
                        .map(|value| JournalEntryId::new(value.journal_id.into_uuid())),
                ),
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
        if let Some(original_expense_journal_id) = existing_journals.first().copied() {
            let mut combined = empty_result(revision.accounting_correlation_id);
            for (index, (direction, control)) in receivables
                .iter()
                .map(|value| (ControlDirection::Receivable, value))
                .chain(
                    payables
                        .iter()
                        .map(|value| (ControlDirection::Payable, value)),
                )
                .enumerate()
            {
                let result = self
                    .ledger
                    .reclassify_expense_to_receivable_or_payable(
                        ReclassifyExpenseToReceivableOrPayable {
                            metadata: ledger_metadata(
                                bill.user_id(),
                                bill.id(),
                                revision.number,
                                revision.accounting_correlation_id,
                                &format!("reclassify:{direction:?}:{index}"),
                            )?,
                            original_expense_journal_id,
                            control_account_id: control.account_id,
                            amount: control.amount.clone(),
                            direction,
                        },
                    )
                    .await
                    .map_err(ledger_error)?;
                merge_result(&mut combined, result);
            }
            if cash.is_empty() {
                return Ok(combined);
            }
            let manual_expense = cash.iter().try_fold(Decimal::ZERO, |sum, value| {
                sum.checked_add(value.amount.amount())
                    .ok_or(SharingError::ArithmeticOverflow)
            })?;
            let result = self
                .ledger
                .record_expense_and_control_balances(RecordExpenseAndControlBalances {
                    metadata: ledger_metadata(
                        bill.user_id(),
                        bill.id(),
                        revision.number,
                        revision.accounting_correlation_id,
                        "manual-remainder",
                    )?,
                    cash_contributions: cash,
                    expense: Money::new(
                        manual_expense,
                        revision.total.currency().clone(),
                        revision.total.amount().scale(),
                    )?,
                    receivables: vec![],
                    payables: vec![],
                    description: revision.title.clone(),
                })
                .await
                .map_err(ledger_error)?;
            merge_result(&mut combined, result);
            return Ok(combined);
        }
        self.ledger
            .record_expense_and_control_balances(RecordExpenseAndControlBalances {
                metadata: ledger_metadata(
                    bill.user_id(),
                    bill.id(),
                    revision.number,
                    revision.accounting_correlation_id,
                    "account",
                )?,
                cash_contributions: cash,
                expense: Money::new(
                    recipe.share,
                    revision.total.currency().clone(),
                    revision.total.amount().scale(),
                )?,
                receivables,
                payables,
                description: revision.title.clone(),
            })
            .await
            .map_err(ledger_error)
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
                    "sharing-bill-accounting-reversal:{}:{}",
                    bill.id(),
                    revision.number
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

fn empty_result(correlation_id: CorrelationId) -> InternalAccountingResult {
    InternalAccountingResult {
        journal_entry_id: None,
        effects: vec![],
        projection_versions: vec![],
        replayed: false,
        cancelled: false,
        outbox_correlation_id: correlation_id,
    }
}

fn merge_result(target: &mut InternalAccountingResult, source: InternalAccountingResult) {
    target.journal_entry_id = source.journal_entry_id.or(target.journal_entry_id);
    target.effects.extend(source.effects);
    target
        .projection_versions
        .extend(source.projection_versions);
    target.replayed |= source.replayed;
    target.cancelled |= source.cancelled;
}

fn ledger_metadata(
    user: UserId,
    bill: BillSplitId,
    revision: u32,
    correlation: CorrelationId,
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
        occurred_at: chrono::Utc::now(),
    })
}
fn ledger_error(error: LedgerError) -> SharingError {
    SharingError::Persistence(error.to_string())
}
