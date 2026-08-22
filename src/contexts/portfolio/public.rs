//! Stable Portfolio commands, queries, and v1 integration events.

pub use super::application::ports::PortfolioLedger;
pub use super::application::{commands::*, queries::*};
pub use super::domain::*;

use super::infrastructure::{PgPortfolioStore, StoreError};
use crate::shared_kernel::{CorrelationId, EventId, UserId};
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

#[derive(Clone)]
pub struct PortfolioFacade {
    pub(crate) store: PgPortfolioStore,
}
impl PortfolioFacade {
    pub(crate) fn new(store: PgPortfolioStore) -> Self {
        Self { store }
    }
    pub async fn create_manual_ovdp(
        &self,
        c: CreateManualOvdpInstrument,
    ) -> Result<PortfolioCommandResult, PortfolioFacadeError> {
        let hash = super::application::commands::canonical_request_hash(
            "create_manual_ovdp",
            c.user_id,
            &c,
        )
        .map_err(|_| PortfolioFacadeError::invalid())?;
        self.store
            .create_instrument(c, hash)
            .await
            .map_err(Into::into)
    }
    pub async fn open_account(
        &self,
        c: OpenPortfolioAccount,
    ) -> Result<PortfolioCommandResult, PortfolioFacadeError> {
        let hash = super::application::commands::canonical_request_hash(
            "open_portfolio_account",
            c.user_id,
            &c,
        )
        .map_err(|_| PortfolioFacadeError::invalid())?;
        self.store.open_account(c, hash).await.map_err(Into::into)
    }
    pub async fn rename_account(
        &self,
        c: ChangePortfolioAccount,
    ) -> Result<PortfolioCommandResult, PortfolioFacadeError> {
        let hash = super::application::commands::canonical_request_hash(
            "rename_portfolio_account",
            c.user_id,
            &c,
        )
        .map_err(|_| PortfolioFacadeError::invalid())?;
        self.store
            .change_account(c, "rename_portfolio_account", None, hash)
            .await
            .map_err(Into::into)
    }
    pub async fn archive_account(
        &self,
        c: ChangePortfolioAccount,
    ) -> Result<PortfolioCommandResult, PortfolioFacadeError> {
        let hash = super::application::commands::canonical_request_hash(
            "archive_portfolio_account",
            c.user_id,
            &c,
        )
        .map_err(|_| PortfolioFacadeError::invalid())?;
        self.store
            .change_account(
                c,
                "archive_portfolio_account",
                Some(AccountLifecycle::Archived),
                hash,
            )
            .await
            .map_err(Into::into)
    }
    pub async fn restore_account(
        &self,
        c: ChangePortfolioAccount,
    ) -> Result<PortfolioCommandResult, PortfolioFacadeError> {
        let hash = super::application::commands::canonical_request_hash(
            "restore_portfolio_account",
            c.user_id,
            &c,
        )
        .map_err(|_| PortfolioFacadeError::invalid())?;
        self.store
            .change_account(
                c,
                "restore_portfolio_account",
                Some(AccountLifecycle::Active),
                hash,
            )
            .await
            .map_err(Into::into)
    }
    pub async fn record_transaction(
        &self,
        c: RecordPortfolioTransaction,
    ) -> Result<PortfolioCommandResult, PortfolioFacadeError> {
        let hash = super::application::commands::canonical_request_hash(
            "record_portfolio_transaction",
            c.user_id,
            &c,
        )
        .map_err(|_| PortfolioFacadeError::invalid())?;
        self.store.record(c, hash).await.map_err(Into::into)
    }
    pub async fn record_valuation(
        &self,
        c: RecordValuationSnapshot,
    ) -> Result<PortfolioCommandResult, PortfolioFacadeError> {
        let hash =
            super::application::commands::canonical_request_hash("record_valuation", c.user_id, &c)
                .map_err(|_| PortfolioFacadeError::invalid())?;
        self.store
            .record_valuation(c, hash)
            .await
            .map_err(Into::into)
    }
    pub async fn reverse_transaction(
        &self,
        c: ReversePortfolioTransaction,
    ) -> Result<PortfolioCommandResult, PortfolioFacadeError> {
        let hash = super::application::commands::canonical_request_hash(
            "reverse_portfolio_transaction",
            c.user_id,
            &c,
        )
        .map_err(|_| PortfolioFacadeError::invalid())?;
        self.store.reverse(c, hash).await.map_err(Into::into)
    }
    pub async fn accounts(
        &self,
        user: UserId,
    ) -> Result<Vec<PortfolioAccountView>, PortfolioFacadeError> {
        self.store.accounts(user).await.map_err(Into::into)
    }
    pub async fn account(
        &self,
        user: UserId,
        id: PortfolioAccountId,
    ) -> Result<Option<PortfolioAccountView>, PortfolioFacadeError> {
        self.store.account(user, id).await.map_err(Into::into)
    }
    pub async fn instruments(
        &self,
        user: UserId,
    ) -> Result<Vec<InstrumentView>, PortfolioFacadeError> {
        self.store.instruments(user).await.map_err(Into::into)
    }
    pub async fn instrument(
        &self,
        user: UserId,
        id: InstrumentId,
    ) -> Result<Option<InstrumentView>, PortfolioFacadeError> {
        self.store.instrument(user, id).await.map_err(Into::into)
    }
    pub async fn positions(
        &self,
        user: UserId,
        account: PortfolioAccountId,
    ) -> Result<Vec<PositionView>, PortfolioFacadeError> {
        self.store
            .positions(user, account)
            .await
            .map_err(Into::into)
    }
    pub async fn activity(
        &self,
        user: UserId,
        account: PortfolioAccountId,
    ) -> Result<Vec<PortfolioTransactionView>, PortfolioFacadeError> {
        self.store.activity(user, account).await.map_err(Into::into)
    }
    pub async fn valuations(
        &self,
        user: UserId,
        account: PortfolioAccountId,
        instrument: InstrumentId,
    ) -> Result<Vec<ValuationView>, PortfolioFacadeError> {
        self.store
            .valuations(user, account, instrument)
            .await
            .map_err(Into::into)
    }
}

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct PortfolioFacadeError {
    kind: PortfolioFacadeErrorKind,
    message: &'static str,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PortfolioFacadeErrorKind {
    NotFound,
    Conflict,
    Invalid,
    Persistence,
}
impl PortfolioFacadeError {
    fn invalid() -> Self {
        Self {
            kind: PortfolioFacadeErrorKind::Invalid,
            message: "invalid portfolio command",
        }
    }
    pub fn is_not_found(&self) -> bool {
        self.kind == PortfolioFacadeErrorKind::NotFound
    }
    pub fn is_conflict(&self) -> bool {
        self.kind == PortfolioFacadeErrorKind::Conflict
    }
    pub fn is_invalid(&self) -> bool {
        self.kind == PortfolioFacadeErrorKind::Invalid
    }
}
impl From<StoreError> for PortfolioFacadeError {
    fn from(v: StoreError) -> Self {
        match v {
            StoreError::NotFound => Self {
                kind: PortfolioFacadeErrorKind::NotFound,
                message: "portfolio fact was not found",
            },
            StoreError::VersionConflict | StoreError::IdempotencyConflict => Self {
                kind: PortfolioFacadeErrorKind::Conflict,
                message: "portfolio command conflicts with current state",
            },
            StoreError::Invalid(_) => Self::invalid(),
            StoreError::Database(_) => Self {
                kind: PortfolioFacadeErrorKind::Persistence,
                message: "portfolio persistence failed",
            },
        }
    }
}

pub const INSTRUMENT_CREATED_V1: &str = "portfolio.instrument-created.v1";
pub const ACCOUNT_CHANGED_V1: &str = "portfolio.account-changed.v1";
pub const TRANSACTION_POSTED_V1: &str = "portfolio.transaction-posted.v1";
pub const TRANSACTION_REVERSED_V1: &str = "portfolio.transaction-reversed.v1";
pub const POSITION_CHANGED_V1: &str = "portfolio.position-changed.v1";
pub const VALUATION_RECORDED_V1: &str = "portfolio.valuation-recorded.v1";
pub const CASH_SETTLEMENT_POSTED_V1: &str = "portfolio.cash-settlement-posted.v1";
pub const CASH_SETTLEMENT_REVERSED_V1: &str = "portfolio.cash-settlement-reversed.v1";
pub const CASH_SETTLEMENT_CANCELLED_V1: &str =
    "portfolio.cash-settlement-cancelled-without-effect.v1";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortfolioEventMetadataV1 {
    pub schema_version: u32,
    pub event_id: EventId,
    pub user_id: UserId,
    pub sequence: u64,
    pub correlation_id: CorrelationId,
    pub occurred_at: DateTime<Utc>,
    pub recorded_at: DateTime<Utc>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PortfolioEventFactV1 {
    InstrumentCreated {
        instrument_id: InstrumentId,
    },
    AccountChanged {
        account_id: PortfolioAccountId,
        lifecycle: AccountLifecycle,
    },
    TransactionPosted {
        transaction_id: PortfolioTransactionId,
        account_id: PortfolioAccountId,
        instrument_id: InstrumentId,
        kind: PortfolioTransactionKind,
        quantity: Decimal,
        currency: String,
    },
    TransactionReversed {
        transaction_id: PortfolioTransactionId,
        original_transaction_id: PortfolioTransactionId,
    },
    PositionChanged {
        account_id: PortfolioAccountId,
        instrument_id: InstrumentId,
        quantity: Decimal,
        known_cost_quantity: Decimal,
        unknown_cost_quantity: Decimal,
        remaining_known_cost: Decimal,
        realized_gain_loss: Option<Decimal>,
        currency: String,
        position_version: u64,
    },
    ValuationRecorded {
        snapshot_id: ValuationSnapshotId,
        account_id: PortfolioAccountId,
        instrument_id: InstrumentId,
        quantity: Decimal,
        price_per_instrument: Decimal,
        accrued_interest_per_instrument: Decimal,
        market_value: Decimal,
        currency: String,
        quoted_at: DateTime<Utc>,
        source: String,
    },
    CashSettlementPosted {
        transaction_id: PortfolioTransactionId,
        journal_id: crate::contexts::ledger::public::JournalEntryId,
    },
    CashSettlementReversed {
        transaction_id: PortfolioTransactionId,
        journal_id: crate::contexts::ledger::public::JournalEntryId,
        reversal_journal_id: crate::contexts::ledger::public::JournalEntryId,
    },
    CashSettlementCancelledWithoutEffect {
        transaction_id: PortfolioTransactionId,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortfolioEventV1 {
    pub metadata: PortfolioEventMetadataV1,
    pub fact: PortfolioEventFactV1,
}

pub const CONTEXT_NAME: &str = "portfolio";
