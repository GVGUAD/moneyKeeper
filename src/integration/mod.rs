//! Reliable, provider-neutral integration primitives for Finance V2.
//!
//! Producers append events to the outbox in their own database transaction.
//! Dispatch is at least once, so consumers use the inbox to make local effects
//! exactly once within the PostgreSQL transaction that owns those effects.

pub mod inbox;
pub mod outbox;
pub mod postgres;
pub mod process_manager;
pub mod process_managers;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::shared_kernel::EventEnvelope;

/// A versioned integration-event envelope and its data-minimal JSON payload.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct IntegrationEvent {
    /// Routing, tenancy, correlation, and version metadata.
    pub envelope: EventEnvelope,
    /// The minimum business facts required by downstream consumers.
    pub payload: Value,
}

impl IntegrationEvent {
    /// Combines envelope metadata with a provider-neutral payload.
    pub fn new(envelope: EventEnvelope, payload: Value) -> Self {
        Self { envelope, payload }
    }
}
