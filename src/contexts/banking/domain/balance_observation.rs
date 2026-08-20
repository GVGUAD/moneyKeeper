//! Immutable provider balance facts and Ledger delivery links.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::contexts::ledger::public::ReconciliationCaseId;
use crate::shared_kernel::{Money, UserId};

use super::{BalanceObservationId, BankingError, ExternalResourceId};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BalanceBasis {
    Reported,
    Available,
    CreditLimit,
    StatementRunning,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "reason")]
pub enum BalanceComparability {
    Comparable(Money),
    NotComparable(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state", content = "case_id")]
pub enum ObservationDelivery {
    Pending,
    Delivered(ReconciliationCaseId),
    IgnoredOlder(ReconciliationCaseId),
    RetryDue,
    TerminalFailure,
    NotComparable,
}

#[derive(Clone, Debug)]
pub struct BalanceObservation {
    id: BalanceObservationId,
    user_id: UserId,
    resource_id: ExternalResourceId,
    source_sequence: i64,
    basis: BalanceBasis,
    provider_money: Money,
    comparability: BalanceComparability,
    observed_at: DateTime<Utc>,
    recorded_at: DateTime<Utc>,
    delivery: ObservationDelivery,
}

impl BalanceObservation {
    #[allow(clippy::too_many_arguments)]
    pub fn record(
        user_id: UserId,
        resource_id: ExternalResourceId,
        source_sequence: i64,
        basis: BalanceBasis,
        provider_money: Money,
        comparability: BalanceComparability,
        observed_at: DateTime<Utc>,
        recorded_at: DateTime<Utc>,
    ) -> Result<Self, BankingError> {
        if source_sequence < 1 || observed_at > recorded_at {
            return Err(BankingError::InvalidValue("invalid balance observation"));
        }
        if let BalanceComparability::Comparable(ref value) = comparability
            && value.currency() != provider_money.currency()
        {
            return Err(BankingError::InvalidValue("observation currency mismatch"));
        }
        let delivery = if matches!(comparability, BalanceComparability::NotComparable(_)) {
            ObservationDelivery::NotComparable
        } else {
            ObservationDelivery::Pending
        };
        Ok(Self {
            id: BalanceObservationId::generate(),
            user_id,
            resource_id,
            source_sequence,
            basis,
            provider_money,
            comparability,
            observed_at,
            recorded_at,
            delivery,
        })
    }
    pub fn mark_delivered(
        &mut self,
        case_id: Option<ReconciliationCaseId>,
    ) -> Result<(), BankingError> {
        if !matches!(
            self.delivery,
            ObservationDelivery::Pending | ObservationDelivery::RetryDue
        ) {
            return Err(BankingError::InvalidState);
        }
        self.delivery = ObservationDelivery::Delivered(case_id.ok_or(
            BankingError::InvalidValue("reconciliation case is required"),
        )?);
        Ok(())
    }
    pub const fn id(&self) -> BalanceObservationId {
        self.id
    }
    pub const fn user_id(&self) -> UserId {
        self.user_id
    }
    pub const fn resource_id(&self) -> ExternalResourceId {
        self.resource_id
    }
    pub const fn source_sequence(&self) -> i64 {
        self.source_sequence
    }
    pub const fn basis(&self) -> BalanceBasis {
        self.basis
    }
    pub fn provider_money(&self) -> &Money {
        &self.provider_money
    }
    pub fn comparable_money(&self) -> Option<&Money> {
        match &self.comparability {
            BalanceComparability::Comparable(value) => Some(value),
            BalanceComparability::NotComparable(_) => None,
        }
    }
    pub const fn observed_at(&self) -> DateTime<Utc> {
        self.observed_at
    }
    pub const fn recorded_at(&self) -> DateTime<Utc> {
        self.recorded_at
    }
    pub fn delivery(&self) -> &ObservationDelivery {
        &self.delivery
    }
}
