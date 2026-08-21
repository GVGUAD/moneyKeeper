//! Stable commands, queries, and versioned events published by Loans.

use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

pub use super::domain::{
    AnnualRate, ComponentBalances, Counterparty, LoanAgreement, LoanAgreementId, LoanDirection,
    LoanError, LoanMovement, LoanMovementId, LoanStatus, LoanTerms, MovementComponents,
    MovementKind, MovementStatus, TermRevision, TermRevisionId,
};
use super::infrastructure::{PgLoansStore, StoreError};
use crate::contexts::ledger::public::{
    AccountKind, AccountNature, ControlAccountRole, JournalEntryId, LedgerAccountId,
};
use crate::shared_kernel::{CorrelationId, CurrencyCode, EventId, IdempotencyKey, UserId};

pub const CONTEXT_NAME: &str = "loans";
pub const AGREEMENT_OPENED_V1: &str = "loans.agreement-opened.v1";
pub const TERMS_REVISED_V1: &str = "loans.terms-revised.v1";
pub const ACCOUNTING_REQUESTED_V1: &str = "loans.accounting-requested.v1";
pub const MOVEMENT_POSTED_V1: &str = "loans.movement-posted.v1";
pub const MOVEMENT_FAILED_V1: &str = "loans.movement-failed.v1";
pub const MOVEMENT_REVERSED_V1: &str = "loans.movement-reversed.v1";
pub const AGREEMENT_CLOSED_V1: &str = "loans.agreement-closed.v1";

#[derive(Clone)]
pub struct LoansFacade {
    pub(crate) store: PgLoansStore,
}

impl LoansFacade {
    pub(crate) fn new(store: PgLoansStore) -> Self {
        Self { store }
    }
    pub async fn open(&self, command: OpenLoan) -> Result<LoanCommandResult, LoansError> {
        let hash = super::application::commands::canonical_request_hash(
            "open_loan",
            "new",
            command.user_id,
            &OpenLoanHash::from(&command),
        )
        .map_err(|_| LoansError::invalid("request"))?;
        self.store.open(command, hash).await.map_err(Into::into)
    }
    pub async fn revise_terms(
        &self,
        command: ReviseLoanTerms,
    ) -> Result<LoanCommandResult, LoansError> {
        let hash = super::application::commands::canonical_request_hash(
            "revise_loan_terms",
            &command.agreement_id.to_string(),
            command.user_id,
            &ReviseLoanTermsHash::from(&command),
        )
        .map_err(|_| LoansError::invalid("request"))?;
        self.store.revise(command, hash).await.map_err(Into::into)
    }
    pub async fn record_movement(
        &self,
        command: RecordLoanMovement,
    ) -> Result<LoanCommandResult, LoansError> {
        let hash = super::application::commands::canonical_request_hash(
            "record_loan_movement",
            &command.agreement_id.to_string(),
            command.user_id,
            &RecordLoanMovementHash::from(&command),
        )
        .map_err(|_| LoansError::invalid("request"))?;
        self.store
            .record_movement(command, hash)
            .await
            .map_err(Into::into)
    }
    pub async fn close(
        &self,
        user: UserId,
        id: LoanAgreementId,
        expected: u64,
        key: IdempotencyKey,
        correlation: CorrelationId,
        now: DateTime<Utc>,
    ) -> Result<LoanCommandResult, LoansError> {
        let body = (expected, correlation, now);
        let hash = super::application::commands::canonical_request_hash(
            "close_loan",
            &id.to_string(),
            user,
            &body,
        )
        .map_err(|_| LoansError::invalid("request"))?;
        self.store
            .close(user, id, expected, key.as_str(), hash, correlation, now)
            .await
            .map_err(Into::into)
    }
    pub async fn list(&self, user: UserId) -> Result<Vec<LoanView>, LoansError> {
        self.store.list(user).await.map_err(Into::into)
    }
    pub async fn get(
        &self,
        user: UserId,
        id: LoanAgreementId,
    ) -> Result<Option<LoanView>, LoansError> {
        self.store.get(user, id).await.map_err(Into::into)
    }
    pub async fn term_revisions(
        &self,
        user: UserId,
        id: LoanAgreementId,
    ) -> Result<Vec<serde_json::Value>, LoansError> {
        self.store
            .term_revisions(user, id)
            .await
            .map_err(Into::into)
    }
    pub async fn movements(
        &self,
        user: UserId,
        id: LoanAgreementId,
    ) -> Result<Vec<LoanMovementView>, LoansError> {
        self.store.movements(user, id).await.map_err(Into::into)
    }
    pub async fn movement(
        &self,
        user: UserId,
        id: LoanAgreementId,
        movement: LoanMovementId,
    ) -> Result<Option<LoanMovementView>, LoansError> {
        self.store
            .movement(user, id, movement)
            .await
            .map_err(Into::into)
    }
    pub async fn pending_openings(&self, limit: i64) -> Result<Vec<LoanView>, LoansError> {
        self.store.pending_openings(limit).await.map_err(Into::into)
    }
    pub async fn confirm_opening(
        &self,
        user: UserId,
        id: LoanAgreementId,
        account: LedgerAccountId,
        now: DateTime<Utc>,
    ) -> Result<(), LoansError> {
        self.store
            .confirm_opening(user, id, account, now)
            .await
            .map_err(Into::into)
    }
    pub async fn fail_opening(
        &self,
        user: UserId,
        id: LoanAgreementId,
        error: &str,
        now: DateTime<Utc>,
    ) -> Result<(), LoansError> {
        self.store
            .fail_opening(user, id, error, now)
            .await
            .map_err(Into::into)
    }
    pub async fn pending_accounting(
        &self,
        limit: i64,
    ) -> Result<Vec<PendingLoanMovement>, LoansError> {
        self.store
            .pending_movements(limit)
            .await
            .map_err(Into::into)
    }
    pub async fn confirm_accounting(
        &self,
        user: UserId,
        agreement: LoanAgreementId,
        movement: LoanMovementId,
        journal: JournalEntryId,
        now: DateTime<Utc>,
    ) -> Result<LoanEventV1, LoansError> {
        self.store
            .confirm_movement(user, agreement, movement, journal, now)
            .await
            .map_err(Into::into)
    }
    pub async fn fail_accounting(
        &self,
        user: UserId,
        agreement: LoanAgreementId,
        movement: LoanMovementId,
        error: &str,
        now: DateTime<Utc>,
    ) -> Result<(), LoansError> {
        self.store
            .fail_movement(user, agreement, movement, error, now)
            .await
            .map_err(Into::into)
    }
    pub async fn request_reversal(
        &self,
        command: RequestLoanReversal,
    ) -> Result<LoanCommandResult, LoansError> {
        let hash = super::application::commands::canonical_request_hash(
            "reverse_loan_movement",
            &format!("{}:{}", command.agreement_id, command.movement_id),
            command.user_id,
            &RequestLoanReversalHash::from(&command),
        )
        .map_err(|_| LoansError::invalid("request"))?;
        self.store
            .request_reversal(command, hash)
            .await
            .map_err(Into::into)
    }
    pub async fn pending_reversals(
        &self,
        limit: i64,
    ) -> Result<Vec<PendingLoanReversal>, LoansError> {
        self.store
            .pending_reversals(limit)
            .await
            .map_err(Into::into)
    }
    pub async fn confirm_reversal(
        &self,
        pending: &PendingLoanReversal,
        reversal: JournalEntryId,
        now: DateTime<Utc>,
    ) -> Result<LoanEventV1, LoansError> {
        self.store
            .confirm_reversal(pending, reversal, now)
            .await
            .map_err(Into::into)
    }
    pub async fn request_replacement(
        &self,
        command: RecordLoanMovement,
        original: LoanMovementId,
    ) -> Result<LoanCommandResult, LoansError> {
        let hash = super::application::commands::canonical_request_hash(
            "replace_loan_movement",
            &format!("{}:{}", command.agreement_id, original),
            command.user_id,
            &RecordLoanMovementHash::from(&command),
        )
        .map_err(|_| LoansError::invalid("request"))?;
        self.store
            .request_replacement(command, original, hash)
            .await
            .map_err(Into::into)
    }
    pub async fn pending_replacements(
        &self,
        limit: i64,
    ) -> Result<Vec<PendingLoanReplacement>, LoansError> {
        self.store
            .pending_replacements(limit)
            .await
            .map_err(Into::into)
    }
    pub async fn confirm_replacement_reversal(
        &self,
        pending: &PendingLoanReplacement,
        reversal: JournalEntryId,
        now: DateTime<Utc>,
    ) -> Result<(), LoansError> {
        self.store
            .confirm_replacement_reversal(pending, reversal, now)
            .await
            .map_err(Into::into)
    }
}

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct LoansError {
    kind: LoansErrorKind,
    message: &'static str,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LoansErrorKind {
    NotFound,
    Conflict,
    Invalid,
    Persistence,
}
impl LoansError {
    fn invalid(_field: &'static str) -> Self {
        Self {
            kind: LoansErrorKind::Invalid,
            message: "invalid loan command",
        }
    }
    pub fn is_not_found(&self) -> bool {
        self.kind == LoansErrorKind::NotFound
    }
    pub fn is_conflict(&self) -> bool {
        self.kind == LoansErrorKind::Conflict
    }
    pub fn is_invalid(&self) -> bool {
        self.kind == LoansErrorKind::Invalid
    }
}
impl From<StoreError> for LoansError {
    fn from(value: StoreError) -> Self {
        match value {
            StoreError::NotFound => Self {
                kind: LoansErrorKind::NotFound,
                message: "loan was not found",
            },
            StoreError::VersionConflict | StoreError::IdempotencyConflict => Self {
                kind: LoansErrorKind::Conflict,
                message: "loan command conflicts with current state",
            },
            StoreError::Invalid(_) => Self {
                kind: LoansErrorKind::Invalid,
                message: "invalid loan command",
            },
            StoreError::Database(_) => Self {
                kind: LoansErrorKind::Persistence,
                message: "loan persistence failed",
            },
        }
    }
}

#[derive(Clone, Debug)]
pub struct OpenLoan {
    pub user_id: UserId,
    pub direction: LoanDirection,
    pub counterparty: String,
    pub contractual_principal: Decimal,
    pub currency: CurrencyCode,
    pub start_date: NaiveDate,
    pub due_date: Option<NaiveDate>,
    pub annual_rate: Option<Decimal>,
    pub idempotency_key: IdempotencyKey,
    pub correlation_id: CorrelationId,
    pub occurred_at: DateTime<Utc>,
}
#[derive(Serialize)]
struct OpenLoanHash {
    direction: LoanDirection,
    counterparty: String,
    #[serde(with = "rust_decimal::serde::str")]
    contractual_principal: Decimal,
    currency: CurrencyCode,
    start_date: NaiveDate,
    due_date: Option<NaiveDate>,
    #[serde(with = "rust_decimal::serde::str_option")]
    annual_rate: Option<Decimal>,
    occurred_at: DateTime<Utc>,
}
impl From<&OpenLoan> for OpenLoanHash {
    fn from(v: &OpenLoan) -> Self {
        Self {
            direction: v.direction,
            counterparty: v.counterparty.clone(),
            contractual_principal: v.contractual_principal,
            currency: v.currency.clone(),
            start_date: v.start_date,
            due_date: v.due_date,
            annual_rate: v.annual_rate,
            occurred_at: v.occurred_at,
        }
    }
}

#[derive(Clone, Debug)]
pub struct ReviseLoanTerms {
    pub user_id: UserId,
    pub agreement_id: LoanAgreementId,
    pub counterparty: String,
    pub contractual_principal: Decimal,
    pub currency: CurrencyCode,
    pub start_date: NaiveDate,
    pub due_date: Option<NaiveDate>,
    pub annual_rate: Option<Decimal>,
    pub reason: String,
    pub expected_version: u64,
    pub idempotency_key: IdempotencyKey,
    pub correlation_id: CorrelationId,
    pub occurred_at: DateTime<Utc>,
}
#[derive(Serialize)]
struct ReviseLoanTermsHash {
    counterparty: String,
    #[serde(with = "rust_decimal::serde::str")]
    contractual_principal: Decimal,
    currency: CurrencyCode,
    start_date: NaiveDate,
    due_date: Option<NaiveDate>,
    #[serde(with = "rust_decimal::serde::str_option")]
    annual_rate: Option<Decimal>,
    reason: String,
    expected_version: u64,
    occurred_at: DateTime<Utc>,
}
impl From<&ReviseLoanTerms> for ReviseLoanTermsHash {
    fn from(v: &ReviseLoanTerms) -> Self {
        Self {
            counterparty: v.counterparty.clone(),
            contractual_principal: v.contractual_principal,
            currency: v.currency.clone(),
            start_date: v.start_date,
            due_date: v.due_date,
            annual_rate: v.annual_rate,
            reason: v.reason.clone(),
            expected_version: v.expected_version,
            occurred_at: v.occurred_at,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MovementAmounts {
    #[serde(with = "rust_decimal::serde::str")]
    pub principal: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub accrued_interest: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub accrued_fee: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub current_interest: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub current_fee: Decimal,
}
impl MovementAmounts {
    pub fn validate(&self) -> Result<(), &'static str> {
        let values = [
            self.principal,
            self.accrued_interest,
            self.accrued_fee,
            self.current_interest,
            self.current_fee,
        ];
        if values.iter().any(Decimal::is_sign_negative) || values.iter().all(Decimal::is_zero) {
            Err("movement_components")
        } else {
            Ok(())
        }
    }
    pub fn total(&self) -> Option<Decimal> {
        [
            self.principal,
            self.accrued_interest,
            self.accrued_fee,
            self.current_interest,
            self.current_fee,
        ]
        .into_iter()
        .try_fold(Decimal::ZERO, Decimal::checked_add)
    }
}

#[derive(Clone, Debug)]
pub struct RecordLoanMovement {
    pub user_id: UserId,
    pub agreement_id: LoanAgreementId,
    pub kind: MovementKind,
    pub currency: CurrencyCode,
    pub amounts: MovementAmounts,
    pub cash_account_id: Option<LedgerAccountId>,
    pub reason: Option<String>,
    pub replaces: Option<LoanMovementId>,
    pub expected_version: u64,
    pub idempotency_key: IdempotencyKey,
    pub correlation_id: CorrelationId,
    pub occurred_at: DateTime<Utc>,
}
#[derive(Serialize)]
struct RecordLoanMovementHash {
    kind: MovementKind,
    currency: CurrencyCode,
    amounts: MovementAmounts,
    cash_account_id: Option<LedgerAccountId>,
    reason: Option<String>,
    replaces: Option<LoanMovementId>,
    expected_version: u64,
    occurred_at: DateTime<Utc>,
}
impl From<&RecordLoanMovement> for RecordLoanMovementHash {
    fn from(v: &RecordLoanMovement) -> Self {
        Self {
            kind: v.kind,
            currency: v.currency.clone(),
            amounts: v.amounts.clone(),
            cash_account_id: v.cash_account_id,
            reason: v.reason.clone(),
            replaces: v.replaces,
            expected_version: v.expected_version,
            occurred_at: v.occurred_at,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoanCommandResult {
    pub agreement_id: LoanAgreementId,
    pub movement_id: Option<LoanMovementId>,
    pub status: String,
    pub version: u64,
    pub replayed: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct LoanView {
    pub id: LoanAgreementId,
    pub user_id: UserId,
    pub direction: LoanDirection,
    pub counterparty: String,
    #[serde(with = "rust_decimal::serde::str")]
    pub contractual_principal: Decimal,
    pub currency: CurrencyCode,
    pub start_date: NaiveDate,
    pub due_date: Option<NaiveDate>,
    #[serde(with = "rust_decimal::serde::str_option")]
    pub annual_rate: Option<Decimal>,
    pub ledger_principal_account_id: Option<LedgerAccountId>,
    pub status: LoanStatus,
    pub balances: MovementAmounts,
    pub version: u64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct LoanMovementView {
    pub id: LoanMovementId,
    pub agreement_id: LoanAgreementId,
    pub kind: MovementKind,
    pub currency: CurrencyCode,
    pub amounts: MovementAmounts,
    pub cash_account_id: Option<LedgerAccountId>,
    pub reason: Option<String>,
    pub status: MovementStatus,
    pub correlation_id: CorrelationId,
    pub ledger_journal_id: Option<JournalEntryId>,
    pub ledger_reversal_id: Option<JournalEntryId>,
    pub replaces: Option<LoanMovementId>,
    pub requested_at: DateTime<Utc>,
}
#[derive(Clone, Debug)]
pub struct PendingLoanMovement {
    pub movement: LoanMovementView,
    pub user_id: UserId,
    pub direction: LoanDirection,
    pub principal_account_id: LedgerAccountId,
    pub agreement_version: u64,
}
#[derive(Clone, Debug)]
pub struct PendingLoanReversal {
    pub pending: PendingLoanMovement,
    pub reason: String,
}
#[derive(Clone, Debug)]
pub struct PendingLoanReplacement {
    pub replacement: PendingLoanMovement,
    pub original_movement_id: LoanMovementId,
    pub original_journal_id: JournalEntryId,
}
#[derive(Clone, Debug)]
pub struct RequestLoanReversal {
    pub user_id: UserId,
    pub agreement_id: LoanAgreementId,
    pub movement_id: LoanMovementId,
    pub reason: String,
    pub expected_version: u64,
    pub idempotency_key: IdempotencyKey,
    pub correlation_id: CorrelationId,
    pub occurred_at: DateTime<Utc>,
}
#[derive(Serialize)]
struct RequestLoanReversalHash {
    reason: String,
    expected_version: u64,
    occurred_at: DateTime<Utc>,
}
impl From<&RequestLoanReversal> for RequestLoanReversalHash {
    fn from(v: &RequestLoanReversal) -> Self {
        Self {
            reason: v.reason.clone(),
            expected_version: v.expected_version,
            occurred_at: v.occurred_at,
        }
    }
}

#[derive(Clone, Debug)]
pub struct LoanOpeningCommand {
    pub user_id: UserId,
    pub agreement_id: LoanAgreementId,
    pub name: String,
    pub currency: CurrencyCode,
    pub direction: LoanDirection,
    pub idempotency_key: IdempotencyKey,
    pub correlation_id: CorrelationId,
    pub occurred_at: DateTime<Utc>,
}
impl LoanOpeningCommand {
    pub const fn account_kind(&self) -> AccountKind {
        match self.direction {
            LoanDirection::Borrowed => AccountKind::LoanPayable,
            LoanDirection::Lent => AccountKind::LoanReceivable,
        }
    }
    pub const fn account_nature(&self) -> AccountNature {
        match self.direction {
            LoanDirection::Borrowed => AccountNature::Liability,
            LoanDirection::Lent => AccountNature::Asset,
        }
    }
}
#[derive(Clone, Debug)]
pub struct LoanAccountingCommand {
    pub pending: PendingLoanMovement,
    pub principal_account_id: LedgerAccountId,
    pub interest_account_role: ControlAccountRole,
    pub fee_account_role: ControlAccountRole,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoanEventMetadataV1 {
    pub schema_version: u32,
    pub event_id: EventId,
    pub user_id: UserId,
    pub sequence: u64,
    pub correlation_id: CorrelationId,
    pub occurred_at: DateTime<Utc>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LoanEventFactV1 {
    AgreementOpened {
        agreement_id: LoanAgreementId,
        direction: LoanDirection,
        counterparty: String,
        #[serde(with = "rust_decimal::serde::str")]
        contractual_principal: Decimal,
        currency: CurrencyCode,
        start_date: NaiveDate,
        due_date: Option<NaiveDate>,
    },
    TermsRevised {
        agreement_id: LoanAgreementId,
        revision: u64,
    },
    MovementRequested {
        agreement_id: LoanAgreementId,
        movement_id: LoanMovementId,
        kind: MovementKind,
        amounts: MovementAmounts,
    },
    MovementPosted {
        agreement_id: LoanAgreementId,
        movement_id: LoanMovementId,
        kind: MovementKind,
        balances: MovementAmounts,
        ledger_journal_id: JournalEntryId,
    },
    MovementFailed {
        agreement_id: LoanAgreementId,
        movement_id: LoanMovementId,
        error_code: String,
    },
    MovementReversed {
        agreement_id: LoanAgreementId,
        movement_id: LoanMovementId,
        balances: MovementAmounts,
        ledger_reversal_id: JournalEntryId,
    },
    AgreementClosed {
        agreement_id: LoanAgreementId,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoanEventV1 {
    pub metadata: LoanEventMetadataV1,
    pub fact: LoanEventFactV1,
}
impl LoanEventV1 {
    pub const fn event_type(&self) -> &'static str {
        match self.fact {
            LoanEventFactV1::AgreementOpened { .. } => AGREEMENT_OPENED_V1,
            LoanEventFactV1::TermsRevised { .. } => TERMS_REVISED_V1,
            LoanEventFactV1::MovementRequested { .. } => ACCOUNTING_REQUESTED_V1,
            LoanEventFactV1::MovementPosted { .. } => MOVEMENT_POSTED_V1,
            LoanEventFactV1::MovementFailed { .. } => MOVEMENT_FAILED_V1,
            LoanEventFactV1::MovementReversed { .. } => MOVEMENT_REVERSED_V1,
            LoanEventFactV1::AgreementClosed { .. } => AGREEMENT_CLOSED_V1,
        }
    }
}
