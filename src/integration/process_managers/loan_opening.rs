//! Durable Loans-to-Ledger account-opening coordinator.

use chrono::Utc;

use crate::contexts::ledger::public::{AccountKind, AccountNature, LedgerFacade, OpenAccount};
use crate::contexts::loans::public::{LoanDirection, LoansFacade};
use crate::shared_kernel::{CorrelationId, IdempotencyKey, Money};

#[derive(Clone)]
pub struct LoanOpeningWorker {
    loans: LoansFacade,
    ledger: LedgerFacade,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LoanOpeningReport {
    pub claimed: bool,
    pub posted: bool,
    pub retry_due: bool,
}

impl LoanOpeningWorker {
    pub fn new(loans: LoansFacade, ledger: LedgerFacade) -> Self {
        Self { loans, ledger }
    }
    pub async fn run_once(&self) -> anyhow::Result<LoanOpeningReport> {
        let Some(loan) = self.loans.pending_openings(1).await?.into_iter().next() else {
            return Ok(LoanOpeningReport::default());
        };
        let key = IdempotencyKey::new(format!("loan-open:{}", loan.id))?;
        let kind = match loan.direction {
            LoanDirection::Borrowed => AccountKind::LoanPayable,
            LoanDirection::Lent => AccountKind::LoanReceivable,
        };
        let nature = match loan.direction {
            LoanDirection::Borrowed => AccountNature::Liability,
            LoanDirection::Lent => AccountNature::Asset,
        };
        let correlation = CorrelationId::generate();
        let now = Utc::now();
        match self
            .ledger
            .open_account(OpenAccount {
                user_id: loan.user_id,
                name: format!("Loan — {}", loan.counterparty),
                currency: loan.currency.clone(),
                kind,
                nature,
                opening_balance: Money::zero(loan.currency, 8)?,
                idempotency_key: key,
                correlation_id: correlation,
                causation_id: None,
                occurred_at: loan.created_at,
            })
            .await
        {
            Ok(result) => {
                self.loans
                    .confirm_opening(loan.user_id, loan.id, result.account.id, now)
                    .await?;
                Ok(LoanOpeningReport {
                    claimed: true,
                    posted: true,
                    retry_due: false,
                })
            }
            Err(_) => Ok(LoanOpeningReport {
                claimed: true,
                posted: false,
                retry_due: true,
            }),
        }
    }
}
