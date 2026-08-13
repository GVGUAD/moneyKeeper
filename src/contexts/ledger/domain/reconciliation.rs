//! Provider-neutral balance observation and reconciliation aggregate.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::shared_kernel::{Money, UserId};

use super::{
    Actor, JournalEntryId, LedgerAccountId, LedgerError, ObservationId, ReconciliationCaseId,
};

/// Monotonic version of an account-balance projection row.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct BalanceVersion(i64);

impl BalanceVersion {
    pub fn new(value: i64) -> Result<Self, LedgerError> {
        if value < 1 {
            return Err(LedgerError::invalid_version());
        }
        Ok(Self(value))
    }
    pub const fn get(self) -> i64 { self.0 }
}

/// Optimistic version of a reconciliation case.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ReconciliationVersion(i64);

impl ReconciliationVersion {
    pub const INITIAL: Self = Self(1);
    pub fn new(value: i64) -> Result<Self, LedgerError> {
        if value < 1 { return Err(LedgerError::invalid_version()); }
        Ok(Self(value))
    }
    pub const fn get(self) -> i64 { self.0 }
    fn next(self) -> Result<Self, LedgerError> {
        self.0.checked_add(1)
            .ok_or_else(|| LedgerError::persistence("reconciliation version overflowed"))
            .and_then(Self::new)
    }
}

/// Opaque source identity without provider-specific model leakage.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceReference {
    source_kind: String,
    stream_id: String,
    item_id: String,
}

impl SourceReference {
    /// Creates a bounded printable source reference.
    pub fn new(
        source_kind: impl Into<String>,
        stream_id: impl Into<String>,
        item_id: impl Into<String>,
    ) -> Result<Self, LedgerError> {
        let source_kind = validate_part(source_kind.into(), 100)?;
        let stream_id = validate_part(stream_id.into(), 300)?;
        let item_id = validate_part(item_id.into(), 300)?;
        Ok(Self { source_kind, stream_id, item_id })
    }

    pub fn source_kind(&self) -> &str { &self.source_kind }
    pub fn stream_id(&self) -> &str { &self.stream_id }
    pub fn item_id(&self) -> &str { &self.item_id }
}

/// Immutable normalized external balance observation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BalanceObservation {
    id: ObservationId,
    source: SourceReference,
    provider_reported: Money,
    available: Option<Money>,
    observed_at: DateTime<Utc>,
    source_sequence: i64,
    recorded_at: DateTime<Utc>,
}

impl BalanceObservation {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: ObservationId,
        source: SourceReference,
        provider_reported: Money,
        available: Option<Money>,
        observed_at: DateTime<Utc>,
        source_sequence: i64,
        recorded_at: DateTime<Utc>,
    ) -> Result<Self, LedgerError> {
        if source_sequence < 0 {
            return Err(LedgerError::invalid_observation(
                "observation source sequence cannot be negative",
            ));
        }
        if available
            .as_ref()
            .is_some_and(|money| money.currency() != provider_reported.currency())
        {
            return Err(LedgerError::currency_mismatch());
        }
        Ok(Self {
            id,
            source,
            provider_reported,
            available,
            observed_at,
            source_sequence,
            recorded_at,
        })
    }

    pub const fn id(&self) -> ObservationId { self.id }
    pub const fn source(&self) -> &SourceReference { &self.source }
    pub const fn provider_reported(&self) -> &Money { &self.provider_reported }
    pub const fn available(&self) -> Option<&Money> { self.available.as_ref() }
    pub const fn observed_at(&self) -> DateTime<Utc> { self.observed_at }
    pub const fn source_sequence(&self) -> i64 { self.source_sequence }
    pub const fn recorded_at(&self) -> DateTime<Utc> { self.recorded_at }
}

/// User-visible reconciliation lifecycle.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReconciliationStatus {
    Matched,
    Pending,
    Superseded,
    IgnoredOlder,
    Approved,
    Dismissed,
    Stale,
}

/// Immutable audit fact emitted by reconciliation transitions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReconciliationEvent {
    Observed { case_id: ReconciliationCaseId, status: ReconciliationStatus },
    Approved { case_id: ReconciliationCaseId, journal_entry_id: JournalEntryId },
    Dismissed { case_id: ReconciliationCaseId },
    Superseded { case_id: ReconciliationCaseId },
    IgnoredOlder { case_id: ReconciliationCaseId },
}

/// Reconciliation case aggregate. It records decisions; it has no balance setter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReconciliationCase {
    id: ReconciliationCaseId,
    user_id: UserId,
    account_id: LedgerAccountId,
    observation: BalanceObservation,
    captured_ledger_balance: Money,
    captured_balance_version: BalanceVersion,
    delta: Money,
    status: ReconciliationStatus,
    version: ReconciliationVersion,
    approval_journal_id: Option<JournalEntryId>,
    decision_actor: Option<Actor>,
    reason: Option<String>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    events: Vec<ReconciliationEvent>,
}

impl ReconciliationCase {
    #[allow(clippy::too_many_arguments)]
    pub fn observe(
        id: ReconciliationCaseId,
        user_id: UserId,
        account_id: LedgerAccountId,
        observation: BalanceObservation,
        captured_ledger_balance: Money,
        captured_balance_version: BalanceVersion,
        actor: Actor,
        now: DateTime<Utc>,
    ) -> Result<Self, LedgerError> {
        if observation.provider_reported.currency() != captured_ledger_balance.currency() {
            return Err(LedgerError::currency_mismatch());
        }
        if matches!(actor, Actor::User(actor_id) if actor_id != user_id) {
            return Err(LedgerError::tenant_mismatch());
        }
        let delta = observation
            .provider_reported
            .checked_sub(&captured_ledger_balance)
            .map_err(|error| LedgerError::invalid_observation(error.to_string()))?;
        let status = if delta.is_zero() {
            ReconciliationStatus::Matched
        } else {
            ReconciliationStatus::Pending
        };
        Ok(Self {
            id,
            user_id,
            account_id,
            observation,
            captured_ledger_balance,
            captured_balance_version,
            delta,
            status,
            version: ReconciliationVersion::INITIAL,
            approval_journal_id: None,
            decision_actor: Some(actor),
            reason: None,
            created_at: now,
            updated_at: now,
            events: vec![ReconciliationEvent::Observed { case_id: id, status }],
        })
    }

    /// Approves a pending case only against the exact observed projection version.
    pub fn approve(
        &mut self,
        expected_version: ReconciliationVersion,
        current_balance_version: BalanceVersion,
        journal_entry_id: JournalEntryId,
        actor: Actor,
        now: DateTime<Utc>,
    ) -> Result<(), LedgerError> {
        self.require_version(expected_version)?;
        if current_balance_version != self.captured_balance_version {
            return Err(LedgerError::stale_observed_balance());
        }
        if self.status != ReconciliationStatus::Pending {
            return Err(LedgerError::invalid_state("only a pending reconciliation can be approved"));
        }
        if matches!(actor, Actor::User(actor_id) if actor_id != self.user_id) {
            return Err(LedgerError::tenant_mismatch());
        }
        self.status = ReconciliationStatus::Approved;
        self.version = self.version.next()?;
        self.approval_journal_id = Some(journal_entry_id);
        self.decision_actor = Some(actor);
        self.updated_at = now;
        self.events.push(ReconciliationEvent::Approved { case_id: self.id, journal_entry_id });
        Ok(())
    }

    /// Dismisses a pending case without posting a journal.
    pub fn dismiss(
        &mut self,
        expected_version: ReconciliationVersion,
        reason: impl Into<String>,
        actor: Actor,
        now: DateTime<Utc>,
    ) -> Result<(), LedgerError> {
        self.require_version(expected_version)?;
        if self.status != ReconciliationStatus::Pending {
            return Err(LedgerError::invalid_state("only a pending reconciliation can be dismissed"));
        }
        let reason = reason.into();
        let reason = reason.trim();
        if reason.is_empty() || reason.chars().count() > 500 {
            return Err(LedgerError::invalid_observation("dismissal reason is invalid"));
        }
        if matches!(actor, Actor::User(actor_id) if actor_id != self.user_id) {
            return Err(LedgerError::tenant_mismatch());
        }
        self.status = ReconciliationStatus::Dismissed;
        self.version = self.version.next()?;
        self.reason = Some(reason.to_owned());
        self.decision_actor = Some(actor);
        self.updated_at = now;
        self.events.push(ReconciliationEvent::Dismissed { case_id: self.id });
        Ok(())
    }

    pub(crate) fn mark_superseded(&mut self, now: DateTime<Utc>) -> Result<(), LedgerError> {
        if self.status != ReconciliationStatus::Pending {
            return Err(LedgerError::invalid_state("only a pending reconciliation can be superseded"));
        }
        self.status = ReconciliationStatus::Superseded;
        self.version = self.version.next()?;
        self.updated_at = now;
        self.events.push(ReconciliationEvent::Superseded { case_id: self.id });
        Ok(())
    }

    fn require_version(&self, expected: ReconciliationVersion) -> Result<(), LedgerError> {
        if self.version != expected { return Err(LedgerError::version_conflict()); }
        Ok(())
    }

    pub const fn id(&self) -> ReconciliationCaseId { self.id }
    pub const fn user_id(&self) -> UserId { self.user_id }
    pub const fn account_id(&self) -> LedgerAccountId { self.account_id }
    pub const fn observation(&self) -> &BalanceObservation { &self.observation }
    pub const fn captured_ledger_balance(&self) -> &Money { &self.captured_ledger_balance }
    pub const fn captured_balance_version(&self) -> BalanceVersion { self.captured_balance_version }
    pub const fn delta(&self) -> &Money { &self.delta }
    pub const fn status(&self) -> ReconciliationStatus { self.status }
    pub const fn version(&self) -> ReconciliationVersion { self.version }
    pub const fn approval_journal_id(&self) -> Option<JournalEntryId> { self.approval_journal_id }
    pub fn reason(&self) -> Option<&str> { self.reason.as_deref() }
    pub const fn created_at(&self) -> DateTime<Utc> { self.created_at }
    pub const fn updated_at(&self) -> DateTime<Utc> { self.updated_at }
    pub fn events(&self) -> &[ReconciliationEvent] { &self.events }
    pub(crate) fn take_events(&mut self) -> Vec<ReconciliationEvent> { std::mem::take(&mut self.events) }
}

fn validate_part(value: String, max: usize) -> Result<String, LedgerError> {
    if value.is_empty()
        || value.trim() != value
        || value.len() > max
        || value.chars().any(char::is_control)
    {
        return Err(LedgerError::invalid_source_reference());
    }
    Ok(value)
}
