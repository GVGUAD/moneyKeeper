//! BillSplit aggregate and immutable revision facts.

use super::{Contribution, Obligation, ParticipantShare, SharingError};
use crate::{
    define_uuid_id,
    shared_kernel::{CorrelationId, Money, UserId},
};
use chrono::{DateTime, Utc};

define_uuid_id!(
    /// Identifies a shared bill aggregate.
    pub BillSplitId
);

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct BillVersion(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountingStatus {
    Pending,
    Posted,
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BillStatus {
    PendingAccounting,
    Active,
    Failed,
    PendingCancellation,
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct BillRevision {
    pub number: u32,
    pub title: String,
    pub occurred_at: DateTime<Utc>,
    pub total: Money,
    pub contributions: Vec<Contribution>,
    pub shares: Vec<ParticipantShare>,
    pub obligations: Vec<Obligation>,
    pub accounting_status: AccountingStatus,
    pub accounting_correlation_id: CorrelationId,
}

impl BillRevision {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        number: u32,
        title: impl AsRef<str>,
        occurred_at: DateTime<Utc>,
        total: Money,
        contributions: Vec<Contribution>,
        shares: Vec<ParticipantShare>,
        obligations: Vec<Obligation>,
        correlation_id: CorrelationId,
    ) -> Result<Self, SharingError> {
        let title = title
            .as_ref()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        if title.is_empty() {
            return Err(SharingError::Empty("bill title"));
        }
        if title.len() > 300 {
            return Err(SharingError::TooLong("bill title"));
        }
        if total.amount() <= rust_decimal::Decimal::ZERO {
            return Err(SharingError::InvalidTotal);
        }
        if contributions.is_empty() || shares.is_empty() {
            return Err(SharingError::Empty("bill allocations"));
        }
        if contributions
            .iter()
            .any(|value| value.amount.currency() != total.currency())
            || shares
                .iter()
                .any(|value| value.amount.currency() != total.currency())
            || obligations
                .iter()
                .any(|value| value.amount.currency() != total.currency())
        {
            return Err(SharingError::CurrencyMismatch);
        }
        let contribution_total = contributions
            .iter()
            .map(|value| value.amount.amount())
            .sum::<rust_decimal::Decimal>();
        let share_total = shares
            .iter()
            .map(|value| value.amount.amount())
            .sum::<rust_decimal::Decimal>();
        if contribution_total != total.amount() {
            return Err(SharingError::ContributionTotalMismatch);
        }
        if share_total != total.amount() {
            return Err(SharingError::ShareTotalMismatch);
        }
        Ok(Self {
            number,
            title,
            occurred_at,
            total,
            contributions,
            shares,
            obligations,
            accounting_status: AccountingStatus::Pending,
            accounting_correlation_id: correlation_id,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct BillSplit {
    id: BillSplitId,
    user_id: UserId,
    revisions: Vec<BillRevision>,
    status: BillStatus,
    version: BillVersion,
    active_settlements: u32,
    cancellation_reason: Option<String>,
}

impl BillSplit {
    pub fn create(
        id: BillSplitId,
        user_id: UserId,
        revision: BillRevision,
    ) -> Result<Self, SharingError> {
        if revision.number != 1 {
            return Err(SharingError::InvalidTransition);
        }
        Ok(Self {
            id,
            user_id,
            revisions: vec![revision],
            status: BillStatus::PendingAccounting,
            version: BillVersion(1),
            active_settlements: 0,
            cancellation_reason: None,
        })
    }
    pub fn rehydrate(
        id: BillSplitId,
        user_id: UserId,
        revisions: Vec<BillRevision>,
        status: BillStatus,
        version: BillVersion,
        active_settlements: u32,
        cancellation_reason: Option<String>,
    ) -> Result<Self, SharingError> {
        if revisions.is_empty() {
            return Err(SharingError::Empty("bill revisions"));
        }
        Ok(Self {
            id,
            user_id,
            revisions,
            status,
            version,
            active_settlements,
            cancellation_reason,
        })
    }
    pub fn mark_accounting_posted(&mut self, expected: BillVersion) -> Result<(), SharingError> {
        self.require_version(expected)?;
        if self.status != BillStatus::PendingAccounting {
            return Err(SharingError::InvalidTransition);
        }
        self.current_revision_mut().accounting_status = AccountingStatus::Posted;
        self.status = BillStatus::Active;
        self.version.0 += 1;
        Ok(())
    }
    pub fn mark_accounting_failed(&mut self, expected: BillVersion) -> Result<(), SharingError> {
        self.require_version(expected)?;
        if self.status != BillStatus::PendingAccounting {
            return Err(SharingError::InvalidTransition);
        }
        self.current_revision_mut().accounting_status = AccountingStatus::Failed;
        self.status = BillStatus::Failed;
        self.version.0 += 1;
        Ok(())
    }
    pub fn revise(
        &mut self,
        mut revision: BillRevision,
        expected: BillVersion,
    ) -> Result<(), SharingError> {
        self.require_version(expected)?;
        self.ensure_changeable()?;
        if revision.total.currency() != self.current_revision().total.currency() {
            return Err(SharingError::CurrencyMismatch);
        }
        revision.number = self.current_revision().number + 1;
        self.revisions.push(revision);
        self.status = BillStatus::PendingAccounting;
        self.version.0 += 1;
        Ok(())
    }
    pub fn request_cancellation(
        &mut self,
        reason: impl AsRef<str>,
        expected: BillVersion,
    ) -> Result<(), SharingError> {
        self.require_version(expected)?;
        self.ensure_changeable()?;
        let reason = reason.as_ref().trim();
        if reason.is_empty() {
            return Err(SharingError::Empty("cancellation reason"));
        }
        self.status = BillStatus::PendingCancellation;
        self.cancellation_reason = Some(reason.to_owned());
        self.version.0 += 1;
        Ok(())
    }
    pub fn confirm_cancelled(&mut self, expected: BillVersion) -> Result<(), SharingError> {
        self.require_version(expected)?;
        if self.status != BillStatus::PendingCancellation {
            return Err(SharingError::InvalidTransition);
        }
        self.status = BillStatus::Cancelled;
        self.version.0 += 1;
        Ok(())
    }
    pub fn register_settlement(&mut self, expected: BillVersion) -> Result<(), SharingError> {
        self.require_version(expected)?;
        if self.status != BillStatus::Active {
            return Err(SharingError::InvalidTransition);
        }
        self.active_settlements = self
            .active_settlements
            .checked_add(1)
            .ok_or(SharingError::ArithmeticOverflow)?;
        self.version.0 += 1;
        Ok(())
    }
    pub fn register_settlement_reversal(
        &mut self,
        expected: BillVersion,
    ) -> Result<(), SharingError> {
        self.require_version(expected)?;
        if self.active_settlements == 0 {
            return Err(SharingError::InvalidTransition);
        }
        self.active_settlements -= 1;
        self.version.0 += 1;
        Ok(())
    }
    fn ensure_changeable(&self) -> Result<(), SharingError> {
        if self.active_settlements > 0 {
            return Err(SharingError::ActiveSettlements);
        }
        if self.status == BillStatus::PendingAccounting {
            return Err(SharingError::AccountingPending);
        }
        if !matches!(self.status, BillStatus::Active | BillStatus::Failed) {
            return Err(SharingError::InvalidTransition);
        }
        Ok(())
    }
    fn require_version(&self, expected: BillVersion) -> Result<(), SharingError> {
        if expected == self.version {
            Ok(())
        } else {
            Err(SharingError::VersionConflict {
                expected: expected.0,
                actual: self.version.0,
            })
        }
    }
    fn current_revision_mut(&mut self) -> &mut BillRevision {
        self.revisions
            .last_mut()
            .expect("BillSplit always has a revision")
    }
    pub fn current_revision(&self) -> &BillRevision {
        self.revisions
            .last()
            .expect("BillSplit always has a revision")
    }
    pub const fn id(&self) -> BillSplitId {
        self.id
    }
    pub const fn user_id(&self) -> UserId {
        self.user_id
    }
    pub fn revisions(&self) -> &[BillRevision] {
        &self.revisions
    }
    pub const fn status(&self) -> BillStatus {
        self.status
    }
    pub const fn version(&self) -> BillVersion {
        self.version
    }
    pub const fn active_settlements(&self) -> u32 {
        self.active_settlements
    }
    pub fn cancellation_reason(&self) -> Option<&str> {
        self.cancellation_reason.as_deref()
    }
}
