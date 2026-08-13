//! Stable Ledger contracts exposed to HTTP adapters and collaborating contexts.

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::shared_kernel::{CausationId, CorrelationId, CurrencyCode, IdempotencyKey, Money, UserId};
use crate::contexts::classification::public::CategoryId;

pub use super::application::accounts::LedgerFacade;

pub use super::domain::{
    AccountAuthority, AccountKind, AccountLifecycle, AccountNature, AccountVersion,
    AccountVisibility, Actor, AnnotationChanged, AnnotationChanges, AnnotationId,
    AnnotationVersion, BalanceObservation, BalanceVersion, BudgetVisibility, CategoryReference,
    JournalEntry, JournalEntryId, JournalRelations, JournalSource, LedgerAccount, LedgerAccountId,
    LedgerError, NormalizedTags, ObservationId, Posting, PostingId, PostingPurpose,
    ReconciliationCase, ReconciliationCaseId, ReconciliationEvent, ReconciliationStatus,
    ReconciliationVersion, SourceReference, SystemAccountRole, TransactionAnnotation,
};

/// Stable bounded-context name used in integration envelopes.
pub const CONTEXT_NAME: &str = "ledger";

/// Opens a manual user-visible Ledger account.
#[derive(Clone, Debug)]
pub struct OpenAccount {
    pub user_id: UserId,
    pub name: String,
    pub currency: CurrencyCode,
    pub kind: AccountKind,
    pub nature: AccountNature,
    pub opening_balance: Money,
    pub idempotency_key: IdempotencyKey,
    pub correlation_id: CorrelationId,
    pub causation_id: Option<CausationId>,
    pub occurred_at: DateTime<Utc>,
}

/// Renames an account with optimistic concurrency and idempotency.
#[derive(Clone, Debug)]
pub struct RenameAccount {
    pub user_id: UserId,
    pub account_id: LedgerAccountId,
    pub name: String,
    pub expected_version: AccountVersion,
    pub idempotency_key: IdempotencyKey,
    pub correlation_id: CorrelationId,
    pub occurred_at: DateTime<Utc>,
}

/// Archives an account without erasing balance or history.
#[derive(Clone, Debug)]
pub struct ArchiveAccount {
    pub user_id: UserId,
    pub account_id: LedgerAccountId,
    pub expected_version: AccountVersion,
    pub idempotency_key: IdempotencyKey,
    pub correlation_id: CorrelationId,
    pub occurred_at: DateTime<Utc>,
}

/// Restores an archived account.
#[derive(Clone, Debug)]
pub struct RestoreAccount {
    pub user_id: UserId,
    pub account_id: LedgerAccountId,
    pub expected_version: AccountVersion,
    pub idempotency_key: IdempotencyKey,
    pub correlation_id: CorrelationId,
    pub occurred_at: DateTime<Utc>,
}

/// Account read model returned by command results.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountView {
    pub id: LedgerAccountId,
    pub user_id: UserId,
    pub name: String,
    pub currency: CurrencyCode,
    pub nature: AccountNature,
    pub kind: AccountKind,
    pub authority: AccountAuthority,
    pub visibility: AccountVisibility,
    pub lifecycle: AccountLifecycle,
    pub version: AccountVersion,
    #[serde(with = "rust_decimal::serde::str")]
    pub signed_balance: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub display_balance: Decimal,
    pub balance_version: i64,
    pub as_of: DateTime<Utc>,
}

/// Durable account-command result.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountResult {
    pub account: AccountView,
    pub opening_journal_id: Option<JournalEntryId>,
    pub replayed: bool,
}

/// Closed user intent translated into controlled double-entry shapes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManualTransactionKind {
    Income,
    Expense,
}

/// Records positive Money as income or expense against one user account.
#[derive(Clone, Debug)]
pub struct RecordManualTransaction {
    pub user_id: UserId,
    pub account_id: LedgerAccountId,
    pub kind: ManualTransactionKind,
    pub amount: Money,
    pub description: String,
    pub category_id: Option<CategoryId>,
    pub note: Option<String>,
    pub tags: NormalizedTags,
    pub budget_visibility: BudgetVisibility,
    pub idempotency_key: IdempotencyKey,
    pub correlation_id: CorrelationId,
    pub causation_id: Option<CausationId>,
    pub occurred_at: DateTime<Utc>,
}

/// One account effect returned after a financial command.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountEffect {
    pub account_id: LedgerAccountId,
    pub currency: CurrencyCode,
    #[serde(with = "rust_decimal::serde::str")]
    pub signed_amount: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub display_effect: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub signed_balance: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub display_balance: Decimal,
    pub balance_version: i64,
}

/// Durable manual-transaction command result.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransactionResult {
    pub journal_entry_id: JournalEntryId,
    pub effects: Vec<AccountEffect>,
    pub annotation_version: AnnotationVersion,
    pub replayed: bool,
}

/// Optional fee charged by a transfer in either represented currency.
#[derive(Clone, Debug)]
pub struct TransferFee {
    pub amount: Money,
}

/// Atomically transfers exact amounts between two user accounts.
#[derive(Clone, Debug)]
pub struct TransferFunds {
    pub user_id: UserId,
    pub source_account_id: LedgerAccountId,
    pub target_account_id: LedgerAccountId,
    pub source_amount: Money,
    pub target_amount: Money,
    pub fee: Option<TransferFee>,
    pub implied_rate: Option<Decimal>,
    pub description: String,
    pub idempotency_key: IdempotencyKey,
    pub correlation_id: CorrelationId,
    pub causation_id: Option<CausationId>,
    pub occurred_at: DateTime<Utc>,
}

/// Durable transfer result with both user-account effects.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferResult {
    pub journal_entry_id: JournalEntryId,
    pub effects: Vec<AccountEffect>,
    #[serde(default, with = "rust_decimal::serde::str_option")]
    pub implied_rate: Option<Decimal>,
    pub replayed: bool,
}
