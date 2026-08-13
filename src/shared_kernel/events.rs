//! Versioned, payload-free event-envelope metadata.

use crate::shared_kernel::{CausationId, CorrelationId, EventId, UserId};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize, de::Error as _};

/// Maximum persisted UTF-8 byte length of an event context name.
pub const MAX_EVENT_CONTEXT_BYTES: usize = 200;
/// Maximum persisted UTF-8 byte length of an aggregate identity.
pub const MAX_EVENT_AGGREGATE_ID_BYTES: usize = 500;
/// Maximum persisted UTF-8 byte length of an event-type name.
pub const MAX_EVENT_TYPE_BYTES: usize = 200;

/// Metadata that identifies and orders a domain or integration event.
///
/// Payloads live at context and integration boundaries, not in the shared
/// kernel, which prevents provider secrets or large source documents from
/// becoming generic envelope fields.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct EventEnvelope {
    event_id: EventId,
    context: String,
    aggregate_id: String,
    aggregate_version: u64,
    event_type: String,
    schema_version: u32,
    user_id: UserId,
    occurred_at: DateTime<Utc>,
    correlation_id: CorrelationId,
    causation_id: Option<CausationId>,
}

#[derive(Deserialize)]
struct SerializedEventEnvelope {
    event_id: EventId,
    context: String,
    aggregate_id: String,
    aggregate_version: u64,
    event_type: String,
    schema_version: u32,
    user_id: UserId,
    occurred_at: DateTime<Utc>,
    correlation_id: CorrelationId,
    causation_id: Option<CausationId>,
}

impl<'de> Deserialize<'de> for EventEnvelope {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let serialized = SerializedEventEnvelope::deserialize(deserializer)?;
        Self::new(
            serialized.event_id,
            serialized.context,
            serialized.aggregate_id,
            serialized.aggregate_version,
            serialized.event_type,
            serialized.schema_version,
            serialized.user_id,
            serialized.occurred_at,
            serialized.correlation_id,
            serialized.causation_id,
        )
        .map_err(D::Error::custom)
    }
}

impl EventEnvelope {
    /// Creates validated, versioned event metadata.
    ///
    /// # Errors
    ///
    /// Returns an error if a text identity is empty, contains control or
    /// surrounding whitespace, exceeds its persisted UTF-8 byte bound, or if
    /// either version is zero.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        event_id: EventId,
        context: impl Into<String>,
        aggregate_id: impl Into<String>,
        aggregate_version: u64,
        event_type: impl Into<String>,
        schema_version: u32,
        user_id: UserId,
        occurred_at: DateTime<Utc>,
        correlation_id: CorrelationId,
        causation_id: Option<CausationId>,
    ) -> Result<Self, EventEnvelopeError> {
        let context = context.into();
        let aggregate_id = aggregate_id.into();
        let event_type = event_type.into();
        validate_identity("context", &context, MAX_EVENT_CONTEXT_BYTES)?;
        validate_identity("aggregate_id", &aggregate_id, MAX_EVENT_AGGREGATE_ID_BYTES)?;
        validate_identity("event_type", &event_type, MAX_EVENT_TYPE_BYTES)?;
        if aggregate_version == 0 {
            return Err(EventEnvelopeError::ZeroAggregateVersion);
        }
        if schema_version == 0 {
            return Err(EventEnvelopeError::ZeroSchemaVersion);
        }
        Ok(Self {
            event_id,
            context,
            aggregate_id,
            aggregate_version,
            event_type,
            schema_version,
            user_id,
            occurred_at,
            correlation_id,
            causation_id,
        })
    }

    /// Returns the immutable event identity.
    pub const fn event_id(&self) -> EventId {
        self.event_id
    }

    /// Returns the bounded-context name that owns the event.
    pub fn context(&self) -> &str {
        &self.context
    }

    /// Returns the context-owned aggregate identity representation.
    pub fn aggregate_id(&self) -> &str {
        &self.aggregate_id
    }

    /// Returns the aggregate version after the event's state change.
    pub const fn aggregate_version(&self) -> u64 {
        self.aggregate_version
    }

    /// Returns the stable event-type name.
    pub fn event_type(&self) -> &str {
        &self.event_type
    }

    /// Returns the version of the event payload contract.
    pub const fn schema_version(&self) -> u32 {
        self.schema_version
    }

    /// Returns the tenant that owns the event.
    pub const fn user_id(&self) -> UserId {
        self.user_id
    }

    /// Returns when the business fact occurred.
    pub const fn occurred_at(&self) -> DateTime<Utc> {
        self.occurred_at
    }

    /// Returns the cross-context workflow correlation identity.
    pub const fn correlation_id(&self) -> CorrelationId {
        self.correlation_id
    }

    /// Returns the direct cause when this event was caused by another event.
    pub const fn causation_id(&self) -> Option<CausationId> {
        self.causation_id
    }
}

fn validate_identity(
    field: &'static str,
    value: &str,
    max_bytes: usize,
) -> Result<(), EventEnvelopeError> {
    if value.is_empty() {
        return Err(EventEnvelopeError::EmptyIdentity { field });
    }
    if value.trim() != value || value.chars().any(char::is_control) {
        return Err(EventEnvelopeError::InvalidIdentity { field });
    }
    if value.len() > max_bytes {
        return Err(EventEnvelopeError::IdentityTooLong {
            field,
            actual_bytes: value.len(),
            max_bytes,
        });
    }
    Ok(())
}

/// Explains why event-envelope metadata was rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum EventEnvelopeError {
    /// A required text identity contained no bytes.
    #[error("event envelope field {field} cannot be empty")]
    EmptyIdentity {
        /// Name of the invalid envelope field.
        field: &'static str,
    },
    /// A text identity contained surrounding whitespace or a control character.
    #[error("event envelope field {field} contains invalid whitespace or control characters")]
    InvalidIdentity {
        /// Name of the invalid envelope field.
        field: &'static str,
    },
    /// A text identity exceeded its persisted UTF-8 byte bound.
    #[error("event envelope field {field} is {actual_bytes} bytes; maximum is {max_bytes}")]
    IdentityTooLong {
        /// Name of the invalid envelope field.
        field: &'static str,
        /// Actual UTF-8 byte length.
        actual_bytes: usize,
        /// Maximum persisted UTF-8 byte length.
        max_bytes: usize,
    },
    /// Aggregate versions begin at one.
    #[error("event aggregate version must be greater than zero")]
    ZeroAggregateVersion,
    /// Event schema versions begin at one.
    #[error("event schema version must be greater than zero")]
    ZeroSchemaVersion,
}
