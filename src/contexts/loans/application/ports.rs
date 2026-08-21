//! Narrow application ports for cross-context accounting.

use std::future::Future;

use crate::contexts::ledger::public::{
    ControlAccountResult, EnsureTypedControlAccount, InternalAccountingResult,
};
use crate::contexts::loans::public::{LoanAccountingCommand, LoanOpeningCommand};

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
