//! Stable public Recurring contracts.
use super::application::{
    commands::canonical_request_hash,
    ports::{MatchAllocation, RecurringRepository, UpdateSubscriptionRecord},
};
pub use super::domain::{
    Allocation, ChargeEvidenceId, ChargeMatching, DecisionSource, MatchId, MatchingEvent,
    MatchingVersion, RecurringError, Subscription, SubscriptionId, SubscriptionStatus,
};
use crate::shared_kernel::UserId;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::{str::FromStr, sync::Arc};
pub const CONTEXT_NAME: &str = "recurring";
pub const CHARGE_MATCHED_V1: &str = "recurring.charge-matched.v1";
pub const CHARGE_UNMATCHED_V1: &str = "recurring.charge-unmatched.v1";
pub const CHARGE_EVIDENCE_RECORDED_V1: &str = "recurring.charge-evidence-recorded.v1";
#[derive(Clone)]
pub struct RecurringFacade {
    repository: Arc<dyn RecurringRepository>,
}
impl RecurringFacade {
    pub(crate) fn new(repository: Arc<dyn RecurringRepository>) -> Self {
        Self { repository }
    }
    pub async fn consume_mail_evidence(
        &self,
        event_id: uuid::Uuid,
        sequence: u64,
        event: crate::contexts::mail::public::ReceiptEvidenceRecordedV1,
    ) -> Result<ConsumeResult, RecurringConsumerError> {
        self.repository
            .consume_mail_evidence(event_id, sequence, event)
            .await
    }
    pub async fn consume_ledger_event(
        &self,
        event: crate::contexts::ledger::public::LedgerEventV1,
    ) -> Result<ConsumeResult, RecurringConsumerError> {
        self.repository.consume_ledger_event(event).await
    }

    pub async fn subscriptions(
        &self,
        user: UserId,
    ) -> Result<Vec<SubscriptionView>, RecurringFacadeError> {
        self.repository.list_subscriptions(user).await
    }

    pub async fn subscription(
        &self,
        user: UserId,
        id: SubscriptionId,
    ) -> Result<Option<SubscriptionView>, RecurringFacadeError> {
        self.repository.get_subscription(user, id.into_uuid()).await
    }

    pub async fn update_subscription(
        &self,
        user: UserId,
        id: SubscriptionId,
        command: UpdateSubscription,
        key: &str,
    ) -> Result<serde_json::Value, RecurringFacadeError> {
        if command.expected_version == 0 {
            return Err(RecurringFacadeError::invalid_without_source(
                "invalid expected_version",
            ));
        }
        if command.status.is_none() && command.category_id.is_none() {
            return Err(RecurringFacadeError::invalid_without_source(
                "subscription patch is empty",
            ));
        }
        let hash = canonical_request_hash("update_subscription", &id.to_string(), user, &command)
            .map_err(|error| {
            RecurringFacadeError::invalid_with_message("invalid request", error)
        })?;
        self.repository
            .update_subscription(UpdateSubscriptionRecord {
                user,
                id: id.into_uuid(),
                expected: command.expected_version,
                status: command.status.as_deref(),
                category_id: command.category_id,
                key,
                hash,
            })
            .await
    }

    pub async fn charges(
        &self,
        user: UserId,
        id: SubscriptionId,
    ) -> Result<Vec<serde_json::Value>, RecurringFacadeError> {
        self.repository.charges(user, id.into_uuid()).await
    }

    pub async fn forecast(
        &self,
        user: UserId,
    ) -> Result<Vec<serde_json::Value>, RecurringFacadeError> {
        self.repository.forecast(user).await
    }

    pub async fn match_charge(
        &self,
        user: UserId,
        evidence_id: ChargeEvidenceId,
        command: MatchCharge,
        key: &str,
    ) -> Result<serde_json::Value, RecurringFacadeError> {
        if command.allocations.is_empty() {
            return Err(RecurringFacadeError::invalid_without_source(
                "allocations are required",
            ));
        }
        let allocations = command
            .allocations
            .iter()
            .map(|allocation| {
                let amount =
                    rust_decimal::Decimal::from_str(&allocation.amount).map_err(|error| {
                        RecurringFacadeError::invalid_with_message(
                            "invalid allocation amount",
                            error,
                        )
                    })?;
                let currency = crate::shared_kernel::CurrencyCode::new(&allocation.currency)
                    .map_err(|error| {
                        RecurringFacadeError::invalid_with_message(
                            "invalid allocation currency",
                            error,
                        )
                    })?;
                Ok(MatchAllocation {
                    journal_entry_id: allocation.journal_entry_id,
                    amount,
                    currency: currency.to_string(),
                })
            })
            .collect::<Result<Vec<_>, RecurringFacadeError>>()?;
        let hash = canonical_request_hash("match_charge", &evidence_id.to_string(), user, &command)
            .map_err(|error| {
                RecurringFacadeError::invalid_with_message("invalid request", error)
            })?;
        self.repository
            .create_match(
                user,
                evidence_id.into_uuid(),
                command.expected_version,
                allocations,
                key,
                hash,
            )
            .await
    }

    pub async fn reject_charge(
        &self,
        user: UserId,
        evidence_id: ChargeEvidenceId,
        command: RejectCharge,
        key: &str,
    ) -> Result<serde_json::Value, RecurringFacadeError> {
        if command.reason.trim().is_empty() {
            return Err(RecurringFacadeError::invalid_without_source(
                "reason is required",
            ));
        }
        let hash =
            canonical_request_hash("reject_charge", &evidence_id.to_string(), user, &command)
                .map_err(|error| {
                    RecurringFacadeError::invalid_with_message("invalid request", error)
                })?;
        self.repository
            .reject(
                user,
                evidence_id.into_uuid(),
                command.expected_version,
                &command.reason,
                key,
                hash,
            )
            .await
    }

    pub async fn unmatch_charge(
        &self,
        user: UserId,
        evidence_id: ChargeEvidenceId,
        match_id: MatchId,
        command: UnmatchCharge,
        key: &str,
    ) -> Result<serde_json::Value, RecurringFacadeError> {
        let target = format!("{evidence_id}:{match_id}");
        let hash =
            canonical_request_hash("unmatch_charge", &target, user, &command).map_err(|error| {
                RecurringFacadeError::invalid_with_message("invalid request", error)
            })?;
        self.repository
            .unmatch(
                user,
                evidence_id.into_uuid(),
                match_id.into_uuid(),
                command.expected_version,
                key,
                hash,
            )
            .await
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct ConsumeResult {
    pub applied: bool,
    pub sequence: u64,
}
#[derive(Debug)]
pub struct RecurringConsumerError {
    rejected: bool,
    message: &'static str,
    source: Option<Box<dyn std::error::Error + Send + Sync>>,
}
impl RecurringConsumerError {
    pub(crate) fn rejected(
        message: &'static str,
        source: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self {
            rejected: true,
            message,
            source: Some(Box::new(source)),
        }
    }
    pub(crate) fn persistence(source: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self {
            rejected: false,
            message: "recurring event persistence failed",
            source: Some(Box::new(source)),
        }
    }
    pub fn is_rejected(&self) -> bool {
        self.rejected
    }
}
impl std::fmt::Display for RecurringConsumerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message)
    }
}
impl std::error::Error for RecurringConsumerError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source.as_deref().map(|source| source as _)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RecurringFacadeErrorKind {
    NotFound,
    VersionConflict,
    IdempotencyConflict,
    CategorizationPending,
    Invalid,
    Persistence,
}
#[derive(Debug)]
pub struct RecurringFacadeError {
    kind: RecurringFacadeErrorKind,
    message: &'static str,
    source: Option<Box<dyn std::error::Error + Send + Sync>>,
}
impl RecurringFacadeError {
    pub(crate) fn not_found(source: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self::with_source(
            RecurringFacadeErrorKind::NotFound,
            "recurring item was not found",
            source,
        )
    }
    pub(crate) fn version_conflict(source: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self::with_source(
            RecurringFacadeErrorKind::VersionConflict,
            "recurring version conflict",
            source,
        )
    }
    pub(crate) fn idempotency_conflict(
        source: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self::with_source(
            RecurringFacadeErrorKind::IdempotencyConflict,
            "recurring idempotency conflict",
            source,
        )
    }
    pub(crate) fn categorization_pending(
        source: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self::with_source(
            RecurringFacadeErrorKind::CategorizationPending,
            "categorization is pending",
            source,
        )
    }
    pub(crate) fn invalid_with_message(
        message: &'static str,
        source: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self::with_source(RecurringFacadeErrorKind::Invalid, message, source)
    }
    pub(crate) fn invalid_without_source(message: &'static str) -> Self {
        Self {
            kind: RecurringFacadeErrorKind::Invalid,
            message,
            source: None,
        }
    }
    pub(crate) fn storage(source: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self::with_source(
            RecurringFacadeErrorKind::Persistence,
            "recurring storage is unavailable",
            source,
        )
    }
    fn with_source(
        kind: RecurringFacadeErrorKind,
        message: &'static str,
        source: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self {
            kind,
            message,
            source: Some(Box::new(source)),
        }
    }
    pub fn is_not_found(&self) -> bool {
        self.kind == RecurringFacadeErrorKind::NotFound
    }
    pub fn is_version_conflict(&self) -> bool {
        self.kind == RecurringFacadeErrorKind::VersionConflict
    }
    pub fn is_idempotency_conflict(&self) -> bool {
        self.kind == RecurringFacadeErrorKind::IdempotencyConflict
    }
    pub fn is_categorization_pending(&self) -> bool {
        self.kind == RecurringFacadeErrorKind::CategorizationPending
    }
    pub fn is_invalid(&self) -> bool {
        self.kind == RecurringFacadeErrorKind::Invalid
    }
    pub fn invalid_reason(&self) -> Option<&'static str> {
        self.is_invalid().then_some(self.message)
    }
}
impl std::fmt::Display for RecurringFacadeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message)
    }
}
impl std::error::Error for RecurringFacadeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source.as_deref().map(|source| source as _)
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

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct UpdateSubscription {
    pub expected_version: u64,
    pub status: Option<String>,
    pub category_id: Option<uuid::Uuid>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct MatchChargeAllocation {
    pub journal_entry_id: uuid::Uuid,
    pub amount: String,
    pub currency: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct MatchCharge {
    pub expected_version: u64,
    pub allocations: Vec<MatchChargeAllocation>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RejectCharge {
    pub expected_version: u64,
    pub reason: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct UnmatchCharge {
    pub expected_version: u64,
}
