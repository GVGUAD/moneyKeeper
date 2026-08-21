//! Retry-safe Loans monetary coordinator. Every Ledger call uses a stable key;
//! Loans confirms component balances only after every required call succeeds.

use chrono::Utc;
use rust_decimal::Decimal;

use crate::contexts::ledger::public::{
    ControlAccountRole, ControlDirection, EnsureTypedControlAccount, ImportProviderTransaction,
    InternalCommandMetadata, LedgerFacade, PrincipalOrAccrual, ProviderTransactionState,
    RecordInterestAndFee, RecordInterestOrFeeAccrual, RecordPrincipalDisbursement,
    RecordPrincipalRepayment, SourceReference, WriteOffLiabilityOrReceivable,
};
use crate::contexts::loans::public::{
    LoanDirection, LoansFacade, MovementKind, PendingLoanMovement,
};
use crate::shared_kernel::{IdempotencyKey, Money};

#[derive(Clone)]
pub struct LoanAccountingWorker {
    loans: LoansFacade,
    ledger: LedgerFacade,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LoanAccountingReport {
    pub claimed: bool,
    pub posted: bool,
    pub retry_due: bool,
}

impl LoanAccountingWorker {
    pub fn new(loans: LoansFacade, ledger: LedgerFacade) -> Self {
        Self { loans, ledger }
    }
    pub async fn run_once(&self) -> anyhow::Result<LoanAccountingReport> {
        let Some(pending) = self.loans.pending_accounting(1).await?.into_iter().next() else {
            return Ok(LoanAccountingReport::default());
        };
        match self.post(&pending).await {
            Ok(journal) => {
                self.loans
                    .confirm_accounting(
                        pending.user_id,
                        pending.movement.agreement_id,
                        pending.movement.id,
                        journal,
                        Utc::now(),
                    )
                    .await?;
                Ok(LoanAccountingReport {
                    claimed: true,
                    posted: true,
                    retry_due: false,
                })
            }
            Err(_) => Ok(LoanAccountingReport {
                claimed: true,
                posted: false,
                retry_due: true,
            }),
        }
    }

    async fn post(
        &self,
        p: &PendingLoanMovement,
    ) -> anyhow::Result<crate::contexts::ledger::public::JournalEntryId> {
        let direction = match p.direction {
            LoanDirection::Borrowed => ControlDirection::Payable,
            LoanDirection::Lent => ControlDirection::Receivable,
        };
        let interest_role = match direction {
            ControlDirection::Payable => ControlAccountRole::InterestPayable,
            ControlDirection::Receivable => ControlAccountRole::InterestReceivable,
        };
        let fee_role = match direction {
            ControlDirection::Payable => ControlAccountRole::FeePayable,
            ControlDirection::Receivable => ControlAccountRole::FeeReceivable,
        };
        let subject = p.movement.agreement_id.to_string();
        let interest = self
            .ledger
            .ensure_typed_control_account(EnsureTypedControlAccount {
                metadata: metadata(p, "ensure-interest")?,
                role: interest_role,
                subject_reference: subject.clone(),
                currency: p.movement.currency.clone(),
            })
            .await?;
        let fee = self
            .ledger
            .ensure_typed_control_account(EnsureTypedControlAccount {
                metadata: metadata(p, "ensure-fee")?,
                role: fee_role,
                subject_reference: subject,
                currency: p.movement.currency.clone(),
            })
            .await?;
        let mut journal = None;
        let a = &p.movement.amounts;
        match p.movement.kind {
            MovementKind::Disbursement => {
                let result = self
                    .ledger
                    .record_principal_disbursement(RecordPrincipalDisbursement {
                        metadata: metadata(p, "principal")?,
                        cash_account_id: p
                            .movement
                            .cash_account_id
                            .expect("validated cash account"),
                        principal_control_account_id: p.principal_account_id,
                        amount: money(p, a.principal)?,
                    })
                    .await?;
                journal = result.journal_entry_id;
            }
            MovementKind::Repayment => {
                if !a.principal.is_zero() {
                    journal = self
                        .ledger
                        .record_principal_repayment(RecordPrincipalRepayment {
                            metadata: metadata(p, "principal")?,
                            cash_account_id: p
                                .movement
                                .cash_account_id
                                .expect("validated cash account"),
                            principal_control_account_id: p.principal_account_id,
                            amount: money(p, a.principal)?,
                        })
                        .await?
                        .journal_entry_id;
                }
                if !a.accrued_interest.is_zero() {
                    journal = self
                        .ledger
                        .record_interest_and_fee(RecordInterestAndFee {
                            metadata: metadata(p, "accrued-interest")?,
                            cash_account_id: p
                                .movement
                                .cash_account_id
                                .expect("validated cash account"),
                            accrual_control_account_id: interest.account_id,
                            amount: money(p, a.accrued_interest)?,
                            component: PrincipalOrAccrual::Interest,
                            direction,
                        })
                        .await?
                        .journal_entry_id;
                }
                if !a.accrued_fee.is_zero() {
                    journal = self
                        .ledger
                        .record_interest_and_fee(RecordInterestAndFee {
                            metadata: metadata(p, "accrued-fee")?,
                            cash_account_id: p
                                .movement
                                .cash_account_id
                                .expect("validated cash account"),
                            accrual_control_account_id: fee.account_id,
                            amount: money(p, a.accrued_fee)?,
                            component: PrincipalOrAccrual::Fee,
                            direction,
                        })
                        .await?
                        .journal_entry_id;
                }
                if !a.current_interest.is_zero() {
                    journal = self
                        .current_cash_component(p, a.current_interest, "current-interest")
                        .await?;
                }
                if !a.current_fee.is_zero() {
                    journal = self
                        .current_cash_component(p, a.current_fee, "current-fee")
                        .await?;
                }
            }
            MovementKind::Accrual => {
                if !a.accrued_interest.is_zero() {
                    journal = self
                        .ledger
                        .record_interest_or_fee_accrual(RecordInterestOrFeeAccrual {
                            metadata: metadata(p, "interest")?,
                            accrual_control_account_id: interest.account_id,
                            amount: money(p, a.accrued_interest)?,
                            component: PrincipalOrAccrual::Interest,
                            direction,
                        })
                        .await?
                        .journal_entry_id;
                }
                if !a.accrued_fee.is_zero() {
                    journal = self
                        .ledger
                        .record_interest_or_fee_accrual(RecordInterestOrFeeAccrual {
                            metadata: metadata(p, "fee")?,
                            accrual_control_account_id: fee.account_id,
                            amount: money(p, a.accrued_fee)?,
                            component: PrincipalOrAccrual::Fee,
                            direction,
                        })
                        .await?
                        .journal_entry_id;
                }
            }
            MovementKind::WriteOff => {
                for (amount, account, component, suffix) in [
                    (
                        a.principal,
                        p.principal_account_id,
                        PrincipalOrAccrual::Principal,
                        "principal",
                    ),
                    (
                        a.accrued_interest,
                        interest.account_id,
                        PrincipalOrAccrual::Interest,
                        "interest",
                    ),
                    (
                        a.accrued_fee,
                        fee.account_id,
                        PrincipalOrAccrual::Fee,
                        "fee",
                    ),
                ] {
                    if !amount.is_zero() {
                        journal = self
                            .ledger
                            .write_off_liability_or_receivable(WriteOffLiabilityOrReceivable {
                                metadata: metadata(p, suffix)?,
                                control_account_id: account,
                                amount: money(p, amount)?,
                                component,
                                direction,
                                reason: p.movement.reason.clone().expect("validated reason"),
                            })
                            .await?
                            .journal_entry_id;
                    }
                }
            }
        }
        journal.ok_or_else(|| anyhow::anyhow!("loan accounting produced no journal"))
    }

    async fn current_cash_component(
        &self,
        p: &PendingLoanMovement,
        amount: Decimal,
        suffix: &str,
    ) -> anyhow::Result<Option<crate::contexts::ledger::public::JournalEntryId>> {
        let signed = if p.direction == LoanDirection::Borrowed {
            -amount
        } else {
            amount
        };
        Ok(self
            .ledger
            .import_provider_transaction(ImportProviderTransaction {
                metadata: metadata(p, suffix)?,
                user_account_id: p.movement.cash_account_id.expect("validated cash account"),
                amount: money(p, signed)?,
                state: ProviderTransactionState::Posted,
                description: format!("Loan {suffix}"),
            })
            .await?
            .journal_entry_id)
    }
}

fn metadata(p: &PendingLoanMovement, suffix: &str) -> anyhow::Result<InternalCommandMetadata> {
    Ok(InternalCommandMetadata {
        user_id: p.user_id,
        source: SourceReference::new(
            "loans",
            p.movement.agreement_id.to_string(),
            p.movement.id.to_string(),
        )?,
        correlation_id: p.movement.correlation_id,
        causation_id: None,
        idempotency_key: IdempotencyKey::new(format!(
            "loan-accounting:{}:{suffix}",
            p.movement.id
        ))?,
        occurred_at: p.movement.requested_at,
    })
}
fn money(p: &PendingLoanMovement, amount: Decimal) -> anyhow::Result<Money> {
    Ok(Money::new(amount, p.movement.currency.clone(), 8)?)
}
