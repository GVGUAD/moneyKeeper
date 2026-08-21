//! Rich Loans domain model.

mod error;
mod loan_agreement;
mod loan_movement;
mod terms;

pub use error::LoanError;
pub use loan_agreement::{
    LoanAgreement, LoanAgreementId, LoanDirection, LoanDomainEvent, LoanStatus,
};
pub use loan_movement::{
    ComponentBalances, LoanMovement, LoanMovementId, MovementComponents, MovementKind,
    MovementStatus,
};
pub use terms::{AnnualRate, Counterparty, LoanTerms, TermRevision, TermRevisionId};
