//! Durable sync job state and in-memory fencing rules.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::shared_kernel::UserId;

use super::{BankingError, ProviderConnectionId, SyncJobId};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncJobState { Requested, Running, WaitingForEvents, RetryDue, Completed, Failed, Cancelled }

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyncLease { holder: String, fencing_token: i64, expires_at: DateTime<Utc> }

#[derive(Clone, Debug)]
pub struct SyncJob {
    id: SyncJobId,
    user_id: UserId,
    connection_id: ProviderConnectionId,
    requested_from: DateTime<Utc>,
    requested_to: DateTime<Utc>,
    state: SyncJobState,
    cursor: Option<String>,
    expected_page_events: usize,
    fencing_token: i64,
    lease_holder: Option<String>,
    lease_expires_at: Option<DateTime<Utc>>,
}

impl SyncJob {
    pub fn request(user_id: UserId, connection_id: ProviderConnectionId, requested_from: DateTime<Utc>, requested_to: DateTime<Utc>) -> Result<Self, BankingError> {
        if requested_from > requested_to { return Err(BankingError::InvalidValue("sync range is inverted")); }
        Ok(Self { id: SyncJobId::generate(), user_id, connection_id, requested_from, requested_to, state: SyncJobState::Requested, cursor: None, expected_page_events: 0, fencing_token: 0, lease_holder: None, lease_expires_at: None })
    }

    pub fn claim(&mut self, holder: impl Into<String>, now: DateTime<Utc>, expires_at: DateTime<Utc>) -> Result<SyncLease, BankingError> {
        let holder = holder.into();
        if holder.is_empty() || expires_at <= now { return Err(BankingError::InvalidValue("invalid sync lease")); }
        if self.lease_expires_at.is_some_and(|expiry| expiry > now) && self.lease_holder.as_deref() != Some(holder.as_str()) { return Err(BankingError::InvalidState); }
        self.fencing_token = self.fencing_token.checked_add(1).ok_or(BankingError::InvalidValue("fencing token overflow"))?;
        self.lease_holder = Some(holder.clone());
        self.lease_expires_at = Some(expires_at);
        self.state = SyncJobState::Running;
        Ok(SyncLease { holder, fencing_token: self.fencing_token, expires_at })
    }

    pub fn begin_page(&mut self, lease: &SyncLease, cursor: impl Into<String>, expected_events: usize, now: DateTime<Utc>) -> Result<(), BankingError> {
        self.require_lease(lease, now)?;
        self.cursor = Some(cursor.into());
        self.expected_page_events = expected_events;
        self.state = SyncJobState::WaitingForEvents;
        Ok(())
    }

    pub fn complete_page(&mut self, lease: &SyncLease, processed: usize, quarantined: usize, now: DateTime<Utc>) -> Result<(), BankingError> {
        self.require_lease(lease, now)?;
        if processed.checked_add(quarantined) != Some(self.expected_page_events) { return Err(BankingError::PageIncomplete); }
        self.state = SyncJobState::Completed;
        Ok(())
    }

    fn require_lease(&self, lease: &SyncLease, now: DateTime<Utc>) -> Result<(), BankingError> {
        if lease.fencing_token != self.fencing_token || self.lease_holder.as_deref() != Some(lease.holder.as_str()) || self.lease_expires_at != Some(lease.expires_at) || lease.expires_at <= now { Err(BankingError::LeaseFenced) } else { Ok(()) }
    }
    pub const fn id(&self) -> SyncJobId { self.id }
    pub const fn user_id(&self) -> UserId { self.user_id }
    pub const fn connection_id(&self) -> ProviderConnectionId { self.connection_id }
    pub const fn requested_from(&self) -> DateTime<Utc> { self.requested_from }
    pub const fn requested_to(&self) -> DateTime<Utc> { self.requested_to }
    pub const fn state(&self) -> SyncJobState { self.state }
    pub fn cursor(&self) -> Option<&str> { self.cursor.as_deref() }
}
