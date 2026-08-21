//! Stable public Recurring contracts.
pub use super::domain::{
    Allocation, ChargeEvidenceId, ChargeMatching, DecisionSource, MatchId, MatchingEvent,
    MatchingVersion, RecurringError, Subscription, SubscriptionId, SubscriptionStatus,
};
use super::infrastructure::PgRecurringStore;
use crate::shared_kernel::UserId;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
pub const CONTEXT_NAME: &str = "recurring";
pub const CHARGE_MATCHED_V1: &str = "recurring.charge-matched.v1";
pub const CHARGE_UNMATCHED_V1: &str = "recurring.charge-unmatched.v1";
pub const CHARGE_EVIDENCE_RECORDED_V1: &str = "recurring.charge-evidence-recorded.v1";
#[derive(Clone)]
pub struct RecurringFacade {
    pub(crate) store: PgRecurringStore,
}
impl RecurringFacade {
    pub(crate) fn new(store: PgRecurringStore) -> Self {
        Self { store }
    }
    pub async fn consume_mail_evidence(
        &self,
        event_id: uuid::Uuid,
        sequence: u64,
        event: crate::contexts::mail::public::ReceiptEvidenceRecordedV1,
    ) -> Result<ConsumeResult, RecurringConsumerError> {
        self.store
            .consume_mail_evidence(event_id, sequence, event)
            .await
            .map_err(RecurringConsumerError::from)
    }
    pub async fn consume_ledger_event(
        &self,
        event: crate::contexts::ledger::public::LedgerEventV1,
    ) -> Result<ConsumeResult, RecurringConsumerError> {
        self.store
            .consume_ledger_event(event)
            .await
            .map_err(RecurringConsumerError::from)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct ConsumeResult {
    pub applied: bool,
    pub sequence: u64,
}
#[derive(Debug, thiserror::Error)]
pub enum RecurringConsumerError {
    #[error("recurring event was rejected: {0}")]
    Rejected(&'static str),
    #[error("recurring event persistence failed")]
    Persistence,
}
impl From<super::infrastructure::StoreError> for RecurringConsumerError {
    fn from(error: super::infrastructure::StoreError) -> Self {
        match error {
            super::infrastructure::StoreError::Invalid(reason) => Self::Rejected(reason),
            super::infrastructure::StoreError::IdempotencyConflict => {
                Self::Rejected("event identity conflict")
            }
            _ => Self::Persistence,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ChargeMatchedV1 {
    pub user_id: UserId,
    pub evidence_id: ChargeEvidenceId,
    pub match_id: MatchId,
    pub allocations: Vec<Allocation>,
    pub occurred_at: DateTime<Utc>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ChargeEvidenceRecordedV1 {
    pub user_id: UserId,
    pub charge_evidence_id: ChargeEvidenceId,
    pub subscription_id: SubscriptionId,
    pub merchant: String,
    pub money: Option<crate::shared_kernel::Money>,
    pub charged_at: Option<DateTime<Utc>>,
    pub recorded_at: DateTime<Utc>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubscriptionView {
    pub id: SubscriptionId,
    pub merchant: String,
    pub status: SubscriptionStatus,
    pub version: u64,
}
