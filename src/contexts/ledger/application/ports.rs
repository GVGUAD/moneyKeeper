//! Transaction-bound Ledger persistence ports.

#![allow(async_fn_in_trait)]

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde_json::Value;

use crate::integration::IntegrationEvent;
use crate::shared_kernel::{CurrencyCode, EventId, IdempotencyKey, UserId};

use super::super::domain::{
    JournalEntry, JournalEntryId, LedgerAccount, LedgerAccountId, LedgerError, Posting,
    ReconciliationCase, ReconciliationCaseId, SourceReference, SystemAccountRole,
    TransactionAnnotation,
};

/// Durable idempotency result read inside a command transaction.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct StoredReceipt {
    pub request_hash: [u8; 32],
    pub status: String,
    pub result: Value,
}

/// Immutable Ledger audit record persisted alongside its aggregate change.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct AuditRecord {
    pub event_id: EventId,
    pub user_id: UserId,
    pub aggregate_kind: &'static str,
    pub aggregate_id: uuid::Uuid,
    pub event_type: &'static str,
    pub actor_kind: &'static str,
    pub actor_reference: Option<String>,
    pub correlation_id: uuid::Uuid,
    pub payload: Value,
    pub occurred_at: DateTime<Utc>,
    pub recorded_at: DateTime<Utc>,
}

/// Starts exactly one PostgreSQL transaction per Ledger command.
pub(crate) trait LedgerUnitOfWork {
    type Tx<'a>: LedgerAccountStore
        + JournalStore
        + AnnotationStore
        + CorrectionStore
        + ReconciliationStore
        + ProjectionStore
        + CommandReceiptStore
        + AuditStore
        + LedgerOutboxStore
        + TransactionControl
    where
        Self: 'a;

    async fn begin(&self) -> Result<Self::Tx<'_>, LedgerError>;
}

#[derive(Clone, Debug)]
pub(crate) struct ReconciliationStream {
    pub latest_observed_at: DateTime<Utc>,
    pub latest_source_sequence: i64,
    pub latest_observation_id: super::super::domain::ObservationId,
    pub active_case_id: Option<ReconciliationCaseId>,
}

pub(crate) trait ReconciliationStore {
    async fn lock_reconciliation_stream(
        &mut self, user_id: UserId, account_id: LedgerAccountId, source: &SourceReference,
    ) -> Result<Option<ReconciliationStream>, LedgerError>;
    async fn find_reconciliation_by_observation(
        &mut self, user_id: UserId, observation_id: super::super::domain::ObservationId,
    ) -> Result<Option<ReconciliationCase>, LedgerError>;
    async fn find_reconciliation_case(
        &mut self, user_id: UserId, case_id: ReconciliationCaseId, lock: bool,
    ) -> Result<Option<ReconciliationCase>, LedgerError>;
    async fn insert_reconciliation_case(&mut self, case: &ReconciliationCase) -> Result<(), LedgerError>;
    async fn save_reconciliation_case(&mut self, case: &ReconciliationCase) -> Result<(), LedgerError>;
    async fn save_reconciliation_stream(
        &mut self, user_id: UserId, account_id: LedgerAccountId, source: &SourceReference,
        stream: &ReconciliationStream, now: DateTime<Utc>,
    ) -> Result<(), LedgerError>;
}

/// Versioned transaction-metadata persistence.
pub(crate) trait AnnotationStore {
    async fn find_annotation(
        &mut self,
        user_id: UserId,
        journal_entry_id: JournalEntryId,
        lock: bool,
    ) -> Result<Option<TransactionAnnotation>, LedgerError>;

    async fn insert_annotation(
        &mut self,
        annotation: &TransactionAnnotation,
    ) -> Result<(), LedgerError>;

    async fn save_annotation(
        &mut self,
        annotation: &TransactionAnnotation,
    ) -> Result<(), LedgerError>;
}

/// Immutable journal facts sufficient to construct a reversal.
#[derive(Clone, Debug)]
pub(crate) struct JournalSnapshot {
    pub id: JournalEntryId,
    pub user_id: UserId,
    pub description: String,
    pub postings: Vec<Posting>,
}

/// Aggregate-shaped account persistence within the caller's transaction.
pub(crate) trait LedgerAccountStore {
    async fn find_account(
        &mut self,
        user_id: UserId,
        id: LedgerAccountId,
        lock: bool,
    ) -> Result<Option<LedgerAccount>, LedgerError>;

    async fn lock_accounts(
        &mut self,
        user_id: UserId,
        ids: &[LedgerAccountId],
    ) -> Result<Vec<LedgerAccount>, LedgerError>;

    async fn insert_account(&mut self, account: &LedgerAccount) -> Result<(), LedgerError>;
    async fn insert_system_account(
        &mut self, account: &LedgerAccount, subject_reference: &str,
    ) -> Result<(), LedgerError>;
    async fn save_account(&mut self, account: &LedgerAccount) -> Result<(), LedgerError>;

    async fn find_system_account(
        &mut self,
        user_id: UserId,
        currency: &CurrencyCode,
        role: SystemAccountRole,
        subject_reference: Option<&str>,
    ) -> Result<Option<LedgerAccount>, LedgerError>;
}

/// Immutable journal aggregate persistence.
pub(crate) trait JournalStore {
    async fn find_journal(
        &mut self,
        user_id: UserId,
        id: JournalEntryId,
        lock: bool,
    ) -> Result<Option<JournalSnapshot>, LedgerError>;

    async fn insert_journal(
        &mut self,
        command_name: &str,
        journal: &JournalEntry,
    ) -> Result<i64, LedgerError>;
}

/// Immutable correction-detail persistence.
pub(crate) struct CorrectionDetail<'a> {
    pub journal_entry_id: JournalEntryId,
    pub user_id: UserId,
    pub account_id: LedgerAccountId,
    pub currency: &'a CurrencyCode,
    pub before_display_balance: Decimal,
    pub target_display_balance: Decimal,
    pub display_delta: Decimal,
    pub observed_balance_version: i64,
    pub reason: &'a str,
    pub observed_at: DateTime<Utc>,
    pub recorded_at: DateTime<Utc>,
}

pub(crate) trait CorrectionStore {
    async fn insert_correction_detail(
        &mut self,
        detail: CorrectionDetail<'_>,
    ) -> Result<(), LedgerError>;
}

/// Rebuildable balance-projection writes bound to the journal transaction.
pub(crate) trait ProjectionStore {
    async fn apply_postings(&mut self, journal: &JournalEntry) -> Result<(), LedgerError>;
    async fn signed_balance(
        &mut self,
        user_id: UserId,
        account_id: LedgerAccountId,
        lock: bool,
    ) -> Result<Option<(Decimal, i64)>, LedgerError>;
}

/// Scoped, payload-sensitive command receipt persistence.
pub(crate) trait CommandReceiptStore {
    async fn find_receipt(
        &mut self,
        user_id: UserId,
        command_name: &str,
        key: &IdempotencyKey,
        lock: bool,
    ) -> Result<Option<StoredReceipt>, LedgerError>;

    async fn insert_receipt(
        &mut self,
        user_id: UserId,
        command_name: &str,
        key: &IdempotencyKey,
        request_hash: &[u8; 32],
        result: &Value,
        completed_at: DateTime<Utc>,
    ) -> Result<(), LedgerError>;
}

/// Append-only audit persistence.
pub(crate) trait AuditStore {
    async fn append_audit(&mut self, record: &AuditRecord) -> Result<(), LedgerError>;
}

/// Transactional outbox persistence owned by the same SQL transaction.
pub(crate) trait LedgerOutboxStore {
    async fn append_outbox(&mut self, event: &IntegrationEvent) -> Result<(), LedgerError>;
}

/// Consumes the transaction to commit or roll it back.
pub(crate) trait TransactionControl: Sized {
    async fn commit(self) -> Result<(), LedgerError>;
    async fn rollback(self) -> Result<(), LedgerError>;
}
