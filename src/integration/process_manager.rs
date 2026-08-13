//! Durable process state, optimistic concurrency, and fenced leases.

use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::Value;

/// Stable identity of one process-manager state machine instance.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ProcessKey {
    process_name: String,
    instance_key: String,
}

impl ProcessKey {
    /// Creates a process key from bounded printable components.
    ///
    /// # Errors
    ///
    /// Returns [`ProcessError::InvalidKey`] for invalid components.
    pub fn new(
        process_name: impl Into<String>,
        instance_key: impl Into<String>,
    ) -> Result<Self, ProcessError> {
        let process_name = process_name.into();
        let instance_key = instance_key.into();
        if !valid_component(&process_name) || !valid_component(&instance_key) {
            return Err(ProcessError::InvalidKey);
        }
        Ok(Self {
            process_name,
            instance_key,
        })
    }

    /// Returns the process definition name.
    pub fn process_name(&self) -> &str {
        &self.process_name
    }

    /// Returns the stable per-workflow instance key.
    pub fn instance_key(&self) -> &str {
        &self.instance_key
    }
}

fn valid_component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 200
        && value.trim() == value
        && !value.chars().any(char::is_control)
}

/// Persisted lifecycle/status label for a process-manager instance.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ProcessStatus(String);

impl ProcessStatus {
    /// Creates a bounded printable status label.
    pub fn new(value: impl Into<String>) -> Result<Self, ProcessError> {
        let value = value.into();
        if !valid_component(&value) {
            return Err(ProcessError::InvalidStatus);
        }
        Ok(Self(value))
    }

    /// Returns the persisted status label.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Caller-owned process state with the version it was loaded at.
#[derive(Clone, Debug, PartialEq)]
pub struct ProcessState {
    key: ProcessKey,
    state: Value,
    status: ProcessStatus,
    version: u64,
    next_wake_at: Option<DateTime<Utc>>,
}

impl ProcessState {
    /// Creates new state whose expected persisted version is zero.
    pub fn new(key: ProcessKey, state: Value, status: ProcessStatus) -> Self {
        Self {
            key,
            state,
            status,
            version: 0,
            next_wake_at: None,
        }
    }

    /// Rehydrates persisted state at `version`.
    pub fn rehydrate(
        key: ProcessKey,
        state: Value,
        status: ProcessStatus,
        version: u64,
        next_wake_at: Option<DateTime<Utc>>,
    ) -> Self {
        Self {
            key,
            state,
            status,
            version,
            next_wake_at,
        }
    }

    /// Replaces state and status while retaining the expected version.
    pub fn transition(&mut self, state: Value, status: ProcessStatus) {
        self.state = state;
        self.status = status;
    }

    /// Schedules or clears the next wake-up while retaining the expected version.
    pub fn set_next_wake_at(&mut self, next_wake_at: Option<DateTime<Utc>>) {
        self.next_wake_at = next_wake_at;
    }

    /// Advances the caller's expected version after a successful save.
    pub fn mark_saved(&mut self) {
        self.version = self.version.saturating_add(1);
    }

    /// Returns the process key.
    pub fn key(&self) -> &ProcessKey {
        &self.key
    }

    /// Returns the opaque, data-minimal process state.
    pub fn state(&self) -> &Value {
        &self.state
    }

    /// Returns the lifecycle label.
    pub fn status(&self) -> &ProcessStatus {
        &self.status
    }

    /// Returns the persisted version this value expects.
    pub const fn version(&self) -> u64 {
        self.version
    }

    /// Returns the optional retry/wake-up instant.
    pub const fn next_wake_at(&self) -> Option<DateTime<Utc>> {
        self.next_wake_at
    }
}

/// Lease whose monotonically increasing token fences expired workers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FencedLease {
    key: ProcessKey,
    holder: String,
    fencing_token: i64,
    expires_at: DateTime<Utc>,
}

impl FencedLease {
    pub(crate) fn rehydrate(
        key: ProcessKey,
        holder: String,
        fencing_token: i64,
        expires_at: DateTime<Utc>,
    ) -> Self {
        Self {
            key,
            holder,
            fencing_token,
            expires_at,
        }
    }

    /// Returns the leased process key.
    pub fn key(&self) -> &ProcessKey {
        &self.key
    }

    /// Returns the stable worker-holder name.
    pub fn holder(&self) -> &str {
        &self.holder
    }

    /// Returns the monotonically increasing fencing token.
    pub const fn fencing_token(&self) -> i64 {
        self.fencing_token
    }

    /// Returns the database-calculated expiry instant.
    pub const fn expires_at(&self) -> DateTime<Utc> {
        self.expires_at
    }
}

/// Failure while leasing or updating a process-manager instance.
#[derive(Debug, thiserror::Error)]
pub enum ProcessError {
    /// Process keys must contain bounded printable components.
    #[error("process key components must contain 1 to 200 printable characters")]
    InvalidKey,
    /// Status labels must be bounded printable strings.
    #[error("process status must contain 1 to 200 printable characters")]
    InvalidStatus,
    /// Lease holder or duration is invalid.
    #[error("invalid process lease request: {0}")]
    InvalidLease(String),
    /// An unexpired lease is owned by another worker.
    #[error("process lease is unavailable")]
    LeaseUnavailable,
    /// The lease expired, was reacquired, or otherwise no longer owns the token.
    #[error("process lease was fenced")]
    LeaseFenced,
    /// State changed since it was loaded.
    #[error("process state version conflict")]
    VersionConflict,
    /// PostgreSQL rejected or could not execute the operation.
    #[error("process-manager persistence failed")]
    Database {
        /// Preserved adapter error without exposing a database type in the port.
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
}

/// Transaction-bound durable process-manager storage.
#[async_trait]
pub trait ProcessManagerStore {
    /// Acquires or renews a lease, advancing its durable fencing token.
    async fn acquire_lease(
        &mut self,
        key: &ProcessKey,
        holder: &str,
        ttl: Duration,
    ) -> Result<FencedLease, ProcessError>;

    /// Saves state only when its version and lease fencing token are current.
    async fn save(&mut self, state: &ProcessState, lease: &FencedLease)
    -> Result<(), ProcessError>;
}
