//! Immutable contractual loan terms and revisions.

use chrono::NaiveDate;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::shared_kernel::Money;

use super::LoanError;

/// Identifies the contractual counterparty without coupling Loans to Sharing.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Counterparty(String);

impl Counterparty {
    /// Creates a bounded, printable counterparty name.
    pub fn new(value: impl Into<String>) -> Result<Self, LoanError> {
        let value = value.into();
        if value.is_empty()
            || value.trim() != value
            || value.len() > 200
            || value.chars().any(char::is_control)
        {
            return Err(LoanError::InvalidValue("counterparty"));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for Counterparty {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Optional simple annual-rate metadata; Loans never derives a schedule from it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnnualRate(#[serde(with = "rust_decimal::serde::str")] Decimal);

impl AnnualRate {
    pub fn new(value: Decimal) -> Result<Self, LoanError> {
        if value.is_sign_negative() {
            return Err(LoanError::InvalidValue("annual_rate"));
        }
        Ok(Self(value))
    }

    pub const fn value(self) -> Decimal {
        self.0
    }
}

/// A complete immutable snapshot of contractual terms.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct LoanTerms {
    counterparty: Counterparty,
    contractual_principal: Money,
    start_date: NaiveDate,
    due_date: Option<NaiveDate>,
    annual_rate: Option<AnnualRate>,
}

impl LoanTerms {
    pub fn new(
        counterparty: Counterparty,
        contractual_principal: Money,
        start_date: NaiveDate,
        due_date: Option<NaiveDate>,
        annual_rate: Option<AnnualRate>,
    ) -> Result<Self, LoanError> {
        if contractual_principal.amount() <= Decimal::ZERO {
            return Err(LoanError::InvalidValue("contractual_principal"));
        }
        if due_date.is_some_and(|due| due < start_date) {
            return Err(LoanError::InvalidValue("due_date"));
        }
        Ok(Self {
            counterparty,
            contractual_principal,
            start_date,
            due_date,
            annual_rate,
        })
    }

    pub const fn counterparty(&self) -> &Counterparty {
        &self.counterparty
    }
    pub const fn contractual_principal(&self) -> &Money {
        &self.contractual_principal
    }
    pub const fn start_date(&self) -> NaiveDate {
        self.start_date
    }
    pub const fn due_date(&self) -> Option<NaiveDate> {
        self.due_date
    }
    pub const fn annual_rate(&self) -> Option<AnnualRate> {
        self.annual_rate
    }
}

crate::define_uuid_id!(#[doc = "Identifies one immutable loan-term revision."] pub TermRevisionId);

/// Append-only contractual terms history.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TermRevision {
    pub id: TermRevisionId,
    pub revision: u64,
    pub terms: LoanTerms,
    pub reason: String,
    pub recorded_at: chrono::DateTime<chrono::Utc>,
}
