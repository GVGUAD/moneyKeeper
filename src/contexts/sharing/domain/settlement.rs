//! Partial settlement aggregate.

use super::{
    BillSplitId, LedgerAccountReference, LedgerJournalReference, Participant, SharingError,
};
use crate::{
    define_uuid_id,
    shared_kernel::{Money, UserId},
};
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;

define_uuid_id!(
    /// Identifies a settlement aggregate.
    pub SettlementId
);
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct SettlementVersion(pub u64);
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SettlementStatus {
    PendingAccounting,
    Posted,
    Failed,
    Reversed,
}
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SettlementEvidence {
    External,
    Manual { account_id: LedgerAccountReference },
    ExistingJournal { journal_id: LedgerJournalReference },
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct Settlement {
    id: SettlementId,
    bill_id: BillSplitId,
    user_id: UserId,
    debtor: Participant,
    creditor: Participant,
    amount: Money,
    evidence: SettlementEvidence,
    status: SettlementStatus,
    version: SettlementVersion,
    occurred_at: DateTime<Utc>,
    reversal_reason: Option<String>,
}

impl Settlement {
    #[allow(clippy::too_many_arguments)]
    pub fn create(
        id: SettlementId,
        bill_id: BillSplitId,
        user_id: UserId,
        debtor: Participant,
        creditor: Participant,
        amount: Money,
        remaining: &Money,
        evidence: SettlementEvidence,
        occurred_at: DateTime<Utc>,
    ) -> Result<Self, SharingError> {
        if debtor == creditor {
            return Err(SharingError::SelfObligation);
        }
        if amount.currency() != remaining.currency() {
            return Err(SharingError::CurrencyMismatch);
        }
        if amount.amount() <= Decimal::ZERO {
            return Err(SharingError::InvalidSettlement);
        }
        if amount.amount() > remaining.amount() {
            return Err(SharingError::OverSettlement);
        }
        if !matches!(debtor, Participant::CurrentUser)
            && !matches!(creditor, Participant::CurrentUser)
            && !matches!(evidence, SettlementEvidence::External)
        {
            return Err(SharingError::InvalidSettlement);
        }
        Ok(Self {
            id,
            bill_id,
            user_id,
            debtor,
            creditor,
            amount,
            evidence,
            status: SettlementStatus::PendingAccounting,
            version: SettlementVersion(1),
            occurred_at,
            reversal_reason: None,
        })
    }
    #[allow(clippy::too_many_arguments)]
    pub fn rehydrate(
        id: SettlementId,
        bill_id: BillSplitId,
        user_id: UserId,
        debtor: Participant,
        creditor: Participant,
        amount: Money,
        evidence: SettlementEvidence,
        status: SettlementStatus,
        version: SettlementVersion,
        occurred_at: DateTime<Utc>,
        reversal_reason: Option<String>,
    ) -> Self {
        Self {
            id,
            bill_id,
            user_id,
            debtor,
            creditor,
            amount,
            evidence,
            status,
            version,
            occurred_at,
            reversal_reason,
        }
    }
    pub fn mark_posted(&mut self, expected: SettlementVersion) -> Result<(), SharingError> {
        self.require_version(expected)?;
        if self.status != SettlementStatus::PendingAccounting {
            return Err(SharingError::InvalidTransition);
        }
        self.status = SettlementStatus::Posted;
        self.version.0 += 1;
        Ok(())
    }
    pub fn mark_failed(&mut self, expected: SettlementVersion) -> Result<(), SharingError> {
        self.require_version(expected)?;
        if self.status != SettlementStatus::PendingAccounting {
            return Err(SharingError::InvalidTransition);
        }
        self.status = SettlementStatus::Failed;
        self.version.0 += 1;
        Ok(())
    }
    pub fn reverse(
        &mut self,
        reason: impl AsRef<str>,
        expected: SettlementVersion,
    ) -> Result<(), SharingError> {
        self.require_version(expected)?;
        if self.status == SettlementStatus::PendingAccounting {
            return Err(SharingError::AccountingPending);
        }
        if self.status == SettlementStatus::Reversed {
            return Err(SharingError::AlreadyReversed);
        }
        let reason = reason.as_ref().trim();
        if reason.is_empty() {
            return Err(SharingError::Empty("reversal reason"));
        }
        self.status = SettlementStatus::Reversed;
        self.reversal_reason = Some(reason.to_owned());
        self.version.0 += 1;
        Ok(())
    }
    fn require_version(&self, expected: SettlementVersion) -> Result<(), SharingError> {
        if expected == self.version {
            Ok(())
        } else {
            Err(SharingError::VersionConflict {
                expected: expected.0,
                actual: self.version.0,
            })
        }
    }
    pub const fn id(&self) -> SettlementId {
        self.id
    }
    pub const fn bill_id(&self) -> BillSplitId {
        self.bill_id
    }
    pub const fn user_id(&self) -> UserId {
        self.user_id
    }
    pub const fn debtor(&self) -> Participant {
        self.debtor
    }
    pub const fn creditor(&self) -> Participant {
        self.creditor
    }
    pub fn amount(&self) -> &Money {
        &self.amount
    }
    pub fn evidence(&self) -> &SettlementEvidence {
        &self.evidence
    }
    pub const fn status(&self) -> SettlementStatus {
        self.status
    }
    pub const fn version(&self) -> SettlementVersion {
        self.version
    }
    pub const fn occurred_at(&self) -> DateTime<Utc> {
        self.occurred_at
    }
    pub fn reversal_reason(&self) -> Option<&str> {
        self.reversal_reason.as_deref()
    }
}
