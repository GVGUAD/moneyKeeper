//! Immutable normalized provider event revisions.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::shared_kernel::{Money, UserId};

use super::{BankingError, ExternalResourceId, ProviderConnectionId, ProviderEventId};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderTransactionState {
    Pending,
    Settled,
    Reversed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventProcessingState {
    Ready,
    WaitingForMapping,
    WaitingForPriorRevision,
    Posting,
    Posted,
    NoFinancialChange,
    RetryDue,
    Quarantined,
    TerminalFailure,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ProviderEventIdentity {
    connection_id: ProviderConnectionId,
    resource_id: ExternalResourceId,
    external_event_id: String,
    revision: i64,
}

impl ProviderEventIdentity {
    pub fn new(
        connection_id: ProviderConnectionId,
        resource_id: ExternalResourceId,
        external_event_id: impl Into<String>,
        revision: i64,
    ) -> Result<Self, BankingError> {
        let external_event_id = external_event_id.into();
        if external_event_id.is_empty() || external_event_id.len() > 200 || revision < 1 {
            return Err(BankingError::InvalidRevision);
        }
        Ok(Self {
            connection_id,
            resource_id,
            external_event_id,
            revision,
        })
    }
    pub const fn connection_id(&self) -> ProviderConnectionId {
        self.connection_id
    }
    pub const fn resource_id(&self) -> ExternalResourceId {
        self.resource_id
    }
    pub fn external_event_id(&self) -> &str {
        &self.external_event_id
    }
    pub const fn revision(&self) -> i64 {
        self.revision
    }
}

#[derive(Clone, Debug)]
pub struct ProviderEvent {
    id: ProviderEventId,
    user_id: UserId,
    identity: ProviderEventIdentity,
    state: ProviderTransactionState,
    operation_money: Money,
    description: String,
    content_digest: [u8; 32],
    effective_at: DateTime<Utc>,
    recorded_at: DateTime<Utc>,
    processing_state: EventProcessingState,
}

impl ProviderEvent {
    #[allow(clippy::too_many_arguments)]
    pub fn record(
        user_id: UserId,
        identity: ProviderEventIdentity,
        state: ProviderTransactionState,
        operation_money: Money,
        description: impl Into<String>,
        content_digest: [u8; 32],
        effective_at: DateTime<Utc>,
        recorded_at: DateTime<Utc>,
    ) -> Result<Self, BankingError> {
        let description = description.into();
        if description.len() > 500
            || description.chars().any(char::is_control)
            || effective_at > recorded_at
        {
            return Err(BankingError::InvalidValue("invalid provider event"));
        }
        Ok(Self {
            id: ProviderEventId::generate(),
            user_id,
            identity,
            state,
            operation_money,
            description,
            content_digest,
            effective_at,
            recorded_at,
            processing_state: EventProcessingState::Ready,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn next_revision(
        &self,
        identity: ProviderEventIdentity,
        state: ProviderTransactionState,
        operation_money: Money,
        content_digest: [u8; 32],
        effective_at: DateTime<Utc>,
        recorded_at: DateTime<Utc>,
    ) -> Result<Self, BankingError> {
        if identity.connection_id != self.identity.connection_id
            || identity.resource_id != self.identity.resource_id
            || identity.external_event_id != self.identity.external_event_id
            || identity.revision != self.identity.revision + 1
        {
            return Err(BankingError::InvalidRevision);
        }
        Self::record(
            self.user_id,
            identity,
            state,
            operation_money,
            self.description.clone(),
            content_digest,
            effective_at,
            recorded_at,
        )
    }

    pub fn is_non_monetary_revision_of(&self, previous: &Self) -> bool {
        self.same_stream(previous)
            && self.operation_money == previous.operation_money
            && self.state != previous.state
    }
    pub fn is_monetary_revision_of(&self, previous: &Self) -> bool {
        self.same_stream(previous) && self.operation_money != previous.operation_money
    }
    fn same_stream(&self, other: &Self) -> bool {
        self.identity.connection_id == other.identity.connection_id
            && self.identity.resource_id == other.identity.resource_id
            && self.identity.external_event_id == other.identity.external_event_id
    }
    pub const fn id(&self) -> ProviderEventId {
        self.id
    }
    pub const fn user_id(&self) -> UserId {
        self.user_id
    }
    pub fn identity(&self) -> &ProviderEventIdentity {
        &self.identity
    }
    pub const fn state(&self) -> ProviderTransactionState {
        self.state
    }
    pub fn operation_money(&self) -> &Money {
        &self.operation_money
    }
    pub fn description(&self) -> &str {
        &self.description
    }
    pub const fn content_digest(&self) -> &[u8; 32] {
        &self.content_digest
    }
    pub const fn effective_at(&self) -> DateTime<Utc> {
        self.effective_at
    }
    pub const fn recorded_at(&self) -> DateTime<Utc> {
        self.recorded_at
    }
    pub const fn processing_state(&self) -> EventProcessingState {
        self.processing_state
    }
}
