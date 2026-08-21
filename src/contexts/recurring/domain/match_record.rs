use super::{ChargeEvidenceId, RecurringError};
use crate::shared_kernel::Money;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
crate::define_uuid_id!(#[doc="Identifies an allocated charge match."] pub MatchId);
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionSource {
    Automatic,
    Manual,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Allocation {
    journal_entry_id: uuid::Uuid,
    amount: Money,
}
impl Allocation {
    pub fn new(journal_entry_id: uuid::Uuid, amount: Money) -> Result<Self, RecurringError> {
        if amount.amount() <= rust_decimal::Decimal::ZERO {
            return Err(RecurringError::InvalidValue("allocation must be positive"));
        }
        Ok(Self {
            journal_entry_id,
            amount,
        })
    }
    pub const fn journal_entry_id(&self) -> uuid::Uuid {
        self.journal_entry_id
    }
    pub fn amount(&self) -> &Money {
        &self.amount
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MatchRecord {
    id: MatchId,
    evidence_id: ChargeEvidenceId,
    allocations: Vec<Allocation>,
    source: DecisionSource,
    created_at: DateTime<Utc>,
}
impl MatchRecord {
    pub(crate) fn record(
        evidence_id: ChargeEvidenceId,
        allocations: Vec<Allocation>,
        source: DecisionSource,
        created_at: DateTime<Utc>,
    ) -> Result<Self, RecurringError> {
        if allocations.is_empty() {
            return Err(RecurringError::InvalidValue("match needs allocations"));
        }
        let currency = allocations[0].amount.currency();
        if allocations.iter().any(|a| a.amount.currency() != currency) {
            return Err(RecurringError::CurrencyMismatch);
        }
        Ok(Self {
            id: MatchId::generate(),
            evidence_id,
            allocations,
            source,
            created_at,
        })
    }
    pub const fn id(&self) -> MatchId {
        self.id
    }
    pub fn allocations(&self) -> &[Allocation] {
        &self.allocations
    }
    pub const fn source(&self) -> DecisionSource {
        self.source
    }
}
