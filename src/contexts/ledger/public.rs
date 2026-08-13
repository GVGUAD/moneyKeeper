//! Stable Ledger contracts exposed to HTTP adapters and collaborating contexts.

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::shared_kernel::{CausationId, CorrelationId, CurrencyCode, EventId, IdempotencyKey, Money, UserId};
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
    #[serde(default, with = "rust_decimal::serde::str_option")]
    pub provider_reported: Option<Decimal>,
    #[serde(default, with = "rust_decimal::serde::str_option")]
    pub available: Option<Decimal>,
    #[serde(default, with = "rust_decimal::serde::str_option")]
    pub reconciliation_difference: Option<Decimal>,
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

/// Corrects a projected display balance through an explicit adjustment journal.
#[derive(Clone, Debug)]
pub struct CorrectBalance {
    pub user_id: UserId,
    pub account_id: LedgerAccountId,
    pub target_display_balance: Money,
    pub expected_balance_version: i64,
    pub reason: String,
    pub observed_at: DateTime<Utc>,
    pub idempotency_key: IdempotencyKey,
    pub correlation_id: CorrelationId,
    pub causation_id: Option<CausationId>,
    pub occurred_at: DateTime<Utc>,
}

/// Creates an exact inverse of an immutable original transaction.
#[derive(Clone, Debug)]
pub struct ReverseTransaction {
    pub user_id: UserId,
    pub journal_entry_id: JournalEntryId,
    pub reason: String,
    pub idempotency_key: IdempotencyKey,
    pub correlation_id: CorrelationId,
    pub causation_id: Option<CausationId>,
    pub occurred_at: DateTime<Utc>,
}

/// Atomically reverses an original and records a corrected manual replacement.
#[derive(Clone, Debug)]
pub struct ReplaceTransaction {
    pub user_id: UserId,
    pub original_journal_entry_id: JournalEntryId,
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

/// Result linking an atomic reversal and replacement.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplacementResult {
    pub reversal_journal_entry_id: JournalEntryId,
    pub replacement_journal_entry_id: JournalEntryId,
    pub effects: Vec<AccountEffect>,
    pub replayed: bool,
}

/// Result of correction or reversal journal creation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FinancialChangeResult {
    pub journal_entry_id: JournalEntryId,
    pub effects: Vec<AccountEffect>,
    pub replayed: bool,
}

/// Version-fenced transaction annotation edit.
#[derive(Clone, Debug)]
pub struct UpdateTransactionAnnotation {
    pub user_id: UserId,
    pub journal_entry_id: JournalEntryId,
    pub changes: AnnotationChanges,
    pub expected_version: AnnotationVersion,
    pub idempotency_key: IdempotencyKey,
    pub correlation_id: CorrelationId,
    pub occurred_at: DateTime<Utc>,
}

/// Result of a transaction annotation mutation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnnotationResult {
    pub journal_entry_id: JournalEntryId,
    pub version: AnnotationVersion,
    pub replayed: bool,
}

/// Stable activity pagination cursor.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivityCursor {
    pub occurred_at: DateTime<Utc>,
    pub ledger_sequence: i64,
}

/// Read-only posting detail.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PostingView {
    pub id: PostingId,
    pub account_id: LedgerAccountId,
    pub position: u16,
    pub currency: CurrencyCode,
    #[serde(with = "rust_decimal::serde::str")]
    pub signed_amount: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub display_effect: Decimal,
}

/// Auditable journal-entry read model.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JournalView {
    pub id: JournalEntryId,
    pub user_id: UserId,
    pub ledger_sequence: i64,
    pub source: JournalSource,
    pub description: String,
    pub occurred_at: DateTime<Utc>,
    pub recorded_at: DateTime<Utc>,
    pub correlation_id: CorrelationId,
    pub relations: JournalRelations,
    pub postings: Vec<PostingView>,
    pub annotation_version: Option<AnnotationVersion>,
}

/// One exact difference between a balance projection and immutable postings.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectionMismatch {
    pub account_id: LedgerAccountId,
    pub user_id: UserId,
    pub currency: CurrencyCode,
    #[serde(with = "rust_decimal::serde::str")]
    pub projected: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub posting_sum: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub delta: Decimal,
}

/// Provider-neutral balance fact submitted by Banking or another adapter.
#[derive(Clone, Debug)]
pub struct ObserveProviderBalance {
    pub user_id: UserId,
    pub account_id: LedgerAccountId,
    pub observation_id: ObservationId,
    pub source: SourceReference,
    pub provider_reported: Money,
    pub available: Option<Money>,
    pub observed_at: DateTime<Utc>,
    pub source_sequence: i64,
    pub idempotency_key: IdempotencyKey,
    pub correlation_id: CorrelationId,
    pub causation_id: Option<CausationId>,
}

/// Approves the exact version and captured projection observed by a case.
#[derive(Clone, Debug)]
pub struct ApproveReconciliation {
    pub user_id: UserId,
    pub case_id: ReconciliationCaseId,
    pub expected_version: ReconciliationVersion,
    pub expected_balance_version: BalanceVersion,
    pub reason: String,
    pub idempotency_key: IdempotencyKey,
    pub correlation_id: CorrelationId,
    pub causation_id: Option<CausationId>,
    pub occurred_at: DateTime<Utc>,
}

/// Dismisses one pending reconciliation without accounting effects.
#[derive(Clone, Debug)]
pub struct DismissReconciliation {
    pub user_id: UserId,
    pub case_id: ReconciliationCaseId,
    pub expected_version: ReconciliationVersion,
    pub reason: String,
    pub idempotency_key: IdempotencyKey,
    pub correlation_id: CorrelationId,
    pub occurred_at: DateTime<Utc>,
}

/// Tenant-scoped reconciliation read model.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ReconciliationView {
    pub id: ReconciliationCaseId,
    pub account_id: LedgerAccountId,
    pub observation_id: ObservationId,
    pub source: SourceReference,
    pub observed_at: DateTime<Utc>,
    pub source_sequence: i64,
    pub provider_reported: Money,
    pub available: Option<Money>,
    pub captured_ledger_balance: Money,
    pub captured_balance_version: BalanceVersion,
    pub delta: Money,
    pub status: ReconciliationStatus,
    pub version: ReconciliationVersion,
    pub approval_journal_id: Option<JournalEntryId>,
    pub reason: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Durable reconciliation command result.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ReconciliationResult {
    pub case: ReconciliationView,
    pub journal_entry_id: Option<JournalEntryId>,
    pub effects: Vec<AccountEffect>,
    pub replayed: bool,
}

/// Common durable process-manager command metadata.
#[derive(Clone, Debug)]
pub struct InternalCommandMetadata {
    pub user_id: UserId,
    pub source: SourceReference,
    pub correlation_id: CorrelationId,
    pub causation_id: Option<CausationId>,
    pub idempotency_key: IdempotencyKey,
    pub occurred_at: DateTime<Utc>,
}

/// Closed Ledger-owned system account roles available to other contexts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlAccountRole {
    ExternalReceivable, ExternalPayable, InterestReceivable, InterestPayable,
    FeeReceivable, FeePayable, PortfolioCashClearing,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlDirection { Receivable, Payable }

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrincipalOrAccrual { Principal, Interest, Fee }

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderTransactionState { Pending, Posted, Reversed }

#[derive(Clone, Debug)]
pub struct EnsureTypedControlAccount {
    pub metadata: InternalCommandMetadata,
    pub role: ControlAccountRole,
    pub subject_reference: String,
    pub currency: CurrencyCode,
}

#[derive(Clone, Debug)]
pub struct ImportProviderTransaction {
    pub metadata: InternalCommandMetadata,
    pub user_account_id: LedgerAccountId,
    pub amount: Money,
    pub state: ProviderTransactionState,
    pub description: String,
}

#[derive(Clone, Debug)]
pub struct TransitionProviderTransactionState {
    pub metadata: InternalCommandMetadata,
    pub imported_journal_entry_id: JournalEntryId,
    pub from: ProviderTransactionState,
    pub to: ProviderTransactionState,
}

#[derive(Clone, Debug)]
pub struct ReverseProviderTransaction {
    pub metadata: InternalCommandMetadata,
    pub imported_journal_entry_id: JournalEntryId,
    pub reason: String,
}

#[derive(Clone, Debug)]
pub struct ReclassifyExpenseToReceivableOrPayable {
    pub metadata: InternalCommandMetadata,
    pub original_expense_journal_id: JournalEntryId,
    pub control_account_id: LedgerAccountId,
    pub amount: Money,
    pub direction: ControlDirection,
}

#[derive(Clone, Debug)]
pub struct SettleReceivableOrPayable {
    pub metadata: InternalCommandMetadata,
    pub user_account_id: LedgerAccountId,
    pub control_account_id: LedgerAccountId,
    pub amount: Money,
    pub direction: ControlDirection,
}

#[derive(Clone, Debug)]
pub struct CashContribution {
    pub account_id: LedgerAccountId,
    pub amount: Money,
}

#[derive(Clone, Debug)]
pub struct ControlAmount {
    pub account_id: LedgerAccountId,
    pub amount: Money,
}

#[derive(Clone, Debug)]
pub struct RecordExpenseAndControlBalances {
    pub metadata: InternalCommandMetadata,
    pub cash_contributions: Vec<CashContribution>,
    pub expense: Money,
    pub receivables: Vec<ControlAmount>,
    pub payables: Vec<ControlAmount>,
    pub description: String,
}

#[derive(Clone, Debug)]
pub struct RecordPrincipalDisbursement {
    pub metadata: InternalCommandMetadata,
    pub cash_account_id: LedgerAccountId,
    pub principal_control_account_id: LedgerAccountId,
    pub amount: Money,
}

#[derive(Clone, Debug)]
pub struct RecordPrincipalRepayment {
    pub metadata: InternalCommandMetadata,
    pub cash_account_id: LedgerAccountId,
    pub principal_control_account_id: LedgerAccountId,
    pub amount: Money,
}

#[derive(Clone, Debug)]
pub struct RecordInterestAndFee {
    pub metadata: InternalCommandMetadata,
    pub cash_account_id: LedgerAccountId,
    pub accrual_control_account_id: LedgerAccountId,
    pub amount: Money,
    pub component: PrincipalOrAccrual,
    pub direction: ControlDirection,
}

#[derive(Clone, Debug)]
pub struct WriteOffLiabilityOrReceivable {
    pub metadata: InternalCommandMetadata,
    pub control_account_id: LedgerAccountId,
    pub amount: Money,
    pub component: PrincipalOrAccrual,
    pub direction: ControlDirection,
    pub reason: String,
}

#[derive(Clone, Debug)]
pub struct RecordCashControlSettlement {
    pub metadata: InternalCommandMetadata,
    pub cash_account_id: LedgerAccountId,
    pub control_account_id: LedgerAccountId,
    pub amount: Money,
    pub source_operation_id: String,
}

#[derive(Clone, Debug)]
pub struct CancelOrReverseCashControlSettlement {
    pub metadata: InternalCommandMetadata,
    pub source_operation_id: String,
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectionVersion {
    pub account_id: LedgerAccountId,
    pub version: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InternalAccountingResult {
    pub journal_entry_id: Option<JournalEntryId>,
    pub effects: Vec<AccountEffect>,
    pub projection_versions: Vec<ProjectionVersion>,
    pub replayed: bool,
    pub cancelled: bool,
    pub outbox_correlation_id: CorrelationId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlAccountResult {
    pub account_id: LedgerAccountId,
    pub role: ControlAccountRole,
    pub subject_reference: String,
    pub currency: CurrencyCode,
    pub replayed: bool,
}

/// Data-minimal money value embedded in versioned integration facts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LedgerMoneyV1 {
    #[serde(with = "rust_decimal::serde::str")]
    pub amount: Decimal,
    pub currency: CurrencyCode,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LedgerEventMetadataV1 {
    pub schema_version: u32,
    pub event_id: EventId,
    pub user_id: UserId,
    pub sequence: u64,
    pub correlation_id: CorrelationId,
    pub causation_id: Option<CausationId>,
    pub occurred_at: DateTime<Utc>,
    pub recorded_at: DateTime<Utc>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LedgerEventFactV1 {
    AccountLifecycleChanged { account_id: LedgerAccountId, lifecycle: AccountLifecycle },
    EntryPosted { journal_entry_id: JournalEntryId, effects: Vec<LedgerMoneyV1> },
    EntryReversed { journal_entry_id: JournalEntryId, original_journal_entry_id: JournalEntryId },
    EntryReplaced { replacement_journal_entry_id: JournalEntryId, original_journal_entry_id: JournalEntryId },
    AnnotationChanged { journal_entry_id: JournalEntryId, version: i64 },
    BalanceChanged { account_id: LedgerAccountId, balance: LedgerMoneyV1, version: i64 },
    ReconciliationObserved { case_id: ReconciliationCaseId },
    ReconciliationMatched { case_id: ReconciliationCaseId },
    ReconciliationSuperseded { case_id: ReconciliationCaseId },
    ReconciliationIgnoredOlder { case_id: ReconciliationCaseId },
    ReconciliationApproved { case_id: ReconciliationCaseId, journal_entry_id: JournalEntryId },
    ReconciliationDismissed { case_id: ReconciliationCaseId },
    ReconciliationStale { case_id: ReconciliationCaseId },
    InternalAccountingCommandPosted { source: SourceReference, journal_entry_id: JournalEntryId },
    InternalAccountingCommandFailed { source: SourceReference, error_code: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LedgerEventV1 {
    pub metadata: LedgerEventMetadataV1,
    pub fact: LedgerEventFactV1,
}
