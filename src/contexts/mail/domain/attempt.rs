//! Append-only fetch and parse attempt facts.
use super::MailError;
use crate::shared_kernel::UserId;
use chrono::{DateTime, Utc};
crate::define_uuid_id!(#[doc = "Identifies a processing attempt."] pub AttemptId);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttemptKind {
    Fetch,
    Parse,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttemptOutcome {
    Succeeded,
    RetryDue,
    Failed,
    Unsupported,
    Malformed,
    DiscardedStale,
    Panicked,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessingAttempt {
    id: AttemptId,
    user_id: UserId,
    kind: AttemptKind,
    outcome: AttemptOutcome,
    processor: String,
    started_at: DateTime<Utc>,
    finished_at: DateTime<Utc>,
}
impl ProcessingAttempt {
    pub fn record(
        user_id: UserId,
        kind: AttemptKind,
        outcome: AttemptOutcome,
        processor: impl Into<String>,
        started_at: DateTime<Utc>,
        finished_at: DateTime<Utc>,
    ) -> Result<Self, MailError> {
        let processor = processor.into();
        if processor.is_empty() || started_at > finished_at {
            return Err(MailError::InvalidValue("invalid processing attempt"));
        }
        Ok(Self {
            id: AttemptId::generate(),
            user_id,
            kind,
            outcome,
            processor,
            started_at,
            finished_at,
        })
    }
    pub const fn id(&self) -> AttemptId {
        self.id
    }
    pub const fn user_id(&self) -> UserId {
        self.user_id
    }
    pub const fn kind(&self) -> AttemptKind {
        self.kind
    }
    pub const fn outcome(&self) -> AttemptOutcome {
        self.outcome
    }
    pub fn processor(&self) -> &str {
        &self.processor
    }
    pub const fn started_at(&self) -> DateTime<Utc> {
        self.started_at
    }
    pub const fn finished_at(&self) -> DateTime<Utc> {
        self.finished_at
    }
}
