use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::domain::receipt_parser::ParsedReceipt;
use crate::domain::subscription::BillingPeriod;

/// Ignored outcomes that can become processable after authentication rules or
/// receipt parsers are upgraded. They are retried only when a user explicitly
/// requests a resync; scheduled mailbox polling must leave them durable.
pub const MANUAL_RESYNC_IGNORED_REASONS: [&str; 4] = [
    "untrusted_sender_authentication",
    "unsupported_sender",
    "not_recurring",
    "recurrence_unknown",
];

pub fn is_manual_resync_ignored_reason(reason: &str) -> bool {
    MANUAL_RESYNC_IGNORED_REASONS.contains(&reason)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncLeaseClaim {
    Acquired,
    Busy,
    NotFound,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MessageIngestionOutcome {
    ChargeCreated(Uuid),
    AlreadyProcessed(Option<Uuid>),
    Tombstoned,
}

#[derive(Debug, Clone)]
pub struct RecurringReceiptIngestion {
    pub connection_id: Uuid,
    pub user_id: Uuid,
    pub provider_message_id: String,
    pub rfc_message_id: Option<String>,
    pub received_at: DateTime<Utc>,
    pub receipt: ParsedReceipt,
    pub billing_period: BillingPeriod,
}

#[derive(Debug, Clone)]
pub struct RetryableEmailMessage {
    pub provider_message_id: String,
    pub attempts: i32,
}

#[derive(Debug, Clone)]
pub struct EmailMessageFailure {
    pub connection_id: Uuid,
    pub user_id: Uuid,
    pub provider_message_id: String,
    pub rfc_message_id: Option<String>,
    pub received_at: DateTime<Utc>,
    pub error_kind: String,
    pub recorded_at: DateTime<Utc>,
}

#[async_trait::async_trait]
pub trait EmailSyncRepository: Send + Sync {
    async fn list_due_connection_ids(
        &self,
        now: DateTime<Utc>,
        limit: i64,
    ) -> anyhow::Result<Vec<Uuid>>;

    async fn claim_connection(
        &self,
        connection_id: Uuid,
        owner: Uuid,
        now: DateTime<Utc>,
        lease_until: DateTime<Utc>,
    ) -> anyhow::Result<SyncLeaseClaim>;

    async fn complete_connection(
        &self,
        connection_id: Uuid,
        owner: Uuid,
        expected_credential_version: i64,
        synced_at: DateTime<Utc>,
        history_id: Option<String>,
        next_sync_at: DateTime<Utc>,
    ) -> anyhow::Result<bool>;

    async fn fail_connection(
        &self,
        connection_id: Uuid,
        owner: Uuid,
        expected_credential_version: i64,
        error_kind: &str,
        now: DateTime<Utc>,
        reconnect_required: bool,
    ) -> anyhow::Result<bool>;

    async fn record_ignored(
        &self,
        connection_id: Uuid,
        user_id: Uuid,
        provider_message_id: &str,
        rfc_message_id: Option<&str>,
        received_at: DateTime<Utc>,
        reason: &str,
    ) -> anyhow::Result<()>;

    async fn record_failure(&self, failure: &EmailMessageFailure) -> anyhow::Result<()>;

    async fn list_retryable_messages(
        &self,
        connection_id: Uuid,
        now: DateTime<Utc>,
        limit: i64,
    ) -> anyhow::Result<Vec<RetryableEmailMessage>>;

    /// Requeue dead letters and ignored messages whose outcome may change
    /// after an authentication or parser upgrade. Implementations must not
    /// requeue permanent ignored outcomes such as tombstones or invalid data.
    async fn requeue_for_manual_resync(
        &self,
        connection_id: Uuid,
        user_id: Uuid,
        now: DateTime<Utc>,
    ) -> anyhow::Result<u64>;

    async fn ingest_recurring(
        &self,
        ingestion: &RecurringReceiptIngestion,
    ) -> anyhow::Result<MessageIngestionOutcome>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manual_resync_reprocesses_only_upgrade_sensitive_ignored_outcomes() {
        for reason in MANUAL_RESYNC_IGNORED_REASONS {
            assert!(is_manual_resync_ignored_reason(reason));
        }

        for permanent_reason in [
            "invalid_recurring_amount_or_currency",
            "subscription_tombstoned",
            "refund",
            "one_time_purchase",
            "promotional",
            "cancellation",
        ] {
            assert!(!is_manual_resync_ignored_reason(permanent_reason));
        }
    }
}
