//! Transaction-bound Ledger persistence ports.

#![allow(async_fn_in_trait)]

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde_json::Value;

use crate::integration::IntegrationEvent;
use crate::shared_kernel::{CurrencyCode, EventId, IdempotencyKey, UserId};

use super::super::domain::{
    JournalEntry, LedgerAccount, LedgerAccountId, LedgerError, SystemAccountRole,
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
        + ProjectionStore
        + CommandReceiptStore
        + AuditStore
        + LedgerOutboxStore
        + TransactionControl
    where
        Self: 'a;

    async fn begin(&self) -> Result<Self::Tx<'_>, LedgerError>;
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
    async fn insert_journal(
        &mut self,
        command_name: &str,
        journal: &JournalEntry,
    ) -> Result<i64, LedgerError>;
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
