use super::super::public::{
    ConsumeResult, RecurringConsumerError, RecurringFacadeError, SubscriptionView,
};
use crate::contexts::ledger::public::{AnnotationResult, LedgerError, UpdateTransactionAnnotation};
use crate::shared_kernel::UserId;
use async_trait::async_trait;
use rust_decimal::Decimal;
use serde_json::Value;
use std::future::Future;
pub(crate) trait AnnotateLedger: Send + Sync {
    fn annotate(
        &self,
        command: UpdateTransactionAnnotation,
    ) -> impl Future<Output = Result<AnnotationResult, LedgerError>> + Send;
}

#[derive(Clone, Debug)]
pub(crate) struct MatchAllocation {
    pub journal_entry_id: uuid::Uuid,
    pub amount: Decimal,
    pub currency: String,
}

pub(crate) struct UpdateSubscriptionRecord<'a> {
    pub user: UserId,
    pub id: uuid::Uuid,
    pub expected: u64,
    pub status: Option<&'a str>,
    pub category_id: Option<uuid::Uuid>,
    pub key: &'a str,
    pub hash: [u8; 32],
}

#[async_trait]
pub(crate) trait RecurringRepository: Send + Sync {
    async fn list_subscriptions(
        &self,
        user: UserId,
    ) -> Result<Vec<SubscriptionView>, RecurringFacadeError>;
    async fn get_subscription(
        &self,
        user: UserId,
        id: uuid::Uuid,
    ) -> Result<Option<SubscriptionView>, RecurringFacadeError>;
    async fn update_subscription(
        &self,
        record: UpdateSubscriptionRecord<'_>,
    ) -> Result<Value, RecurringFacadeError>;
    async fn charges(
        &self,
        user: UserId,
        subscription_id: uuid::Uuid,
    ) -> Result<Vec<Value>, RecurringFacadeError>;
    async fn forecast(&self, user: UserId) -> Result<Vec<Value>, RecurringFacadeError>;
    async fn create_match(
        &self,
        user: UserId,
        evidence_id: uuid::Uuid,
        expected: u64,
        allocations: Vec<MatchAllocation>,
        key: &str,
        hash: [u8; 32],
    ) -> Result<Value, RecurringFacadeError>;
    async fn reject(
        &self,
        user: UserId,
        evidence_id: uuid::Uuid,
        expected: u64,
        reason: &str,
        key: &str,
        hash: [u8; 32],
    ) -> Result<Value, RecurringFacadeError>;
    async fn unmatch(
        &self,
        user: UserId,
        evidence_id: uuid::Uuid,
        match_id: uuid::Uuid,
        expected: u64,
        key: &str,
        hash: [u8; 32],
    ) -> Result<Value, RecurringFacadeError>;
    async fn consume_mail_evidence(
        &self,
        event_id: uuid::Uuid,
        sequence: u64,
        event: crate::contexts::mail::public::ReceiptEvidenceRecordedV1,
    ) -> Result<ConsumeResult, RecurringConsumerError>;
    async fn consume_ledger_event(
        &self,
        event: crate::contexts::ledger::public::LedgerEventV1,
    ) -> Result<ConsumeResult, RecurringConsumerError>;
}
