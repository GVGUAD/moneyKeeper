//! Stable Sharing contracts exposed to HTTP, Reporting, and process managers.

pub use super::application::SharingFacade;
pub use super::application::commands::*;
pub use super::application::queries::*;
pub use super::domain::{
    AccountingStatus, BillRevision, BillSplit, BillSplitId, BillStatus, BillVersion, Contact,
    ContactId, ContactName, ContactStatus, ContactVersion, Contribution, ContributionEvidence,
    ExactShare, JournalAllocation, LedgerAccountReference, LedgerJournalReference, Obligation,
    Participant, ParticipantShare, Settlement, SettlementEvidence, SettlementId, SettlementStatus,
    SettlementVersion, ShareRequest, SharingError, derive_obligations, resolve_allocations,
};

use crate::shared_kernel::{CausationId, CorrelationId, CurrencyCode, EventId, UserId};
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

pub const CONTEXT_NAME: &str = "sharing";
pub const ACCOUNTING_REQUESTED_V1: &str = "sharing.accounting-requested.v1";
pub const SETTLEMENT_ACCOUNTING_REQUESTED_V1: &str = "sharing.settlement-accounting-requested.v1";
pub const BILL_CANCELLED_V1: &str = "sharing.bill-cancelled.v1";
pub const BILL_POSITION_CHANGED_V1: &str = "sharing.bill-position-changed.v1";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SharingEventMetadataV1 {
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
#[serde(tag = "kind", content = "contact_id", rename_all = "snake_case")]
pub enum ParticipantV1 {
    CurrentUser,
    Contact(ContactId),
}

impl From<Participant> for ParticipantV1 {
    fn from(value: Participant) -> Self {
        match value {
            Participant::CurrentUser => Self::CurrentUser,
            Participant::Contact(id) => Self::Contact(id),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BillPositionV1 {
    pub bill_id: BillSplitId,
    pub revision: u32,
    pub currency: CurrencyCode,
    #[serde(with = "rust_decimal::serde::str")]
    pub receivable: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub payable: Decimal,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SharingEventFactV1 {
    BillPositionChanged {
        position: BillPositionV1,
    },
    SettlementPosted {
        bill_id: BillSplitId,
        settlement_id: SettlementId,
        debtor: ParticipantV1,
        creditor: ParticipantV1,
        #[serde(with = "rust_decimal::serde::str")]
        amount: Decimal,
        currency: CurrencyCode,
    },
    SettlementReversed {
        bill_id: BillSplitId,
        settlement_id: SettlementId,
    },
    BillCancelled {
        bill_id: BillSplitId,
        revision: u32,
        bill_version: BillVersion,
        reason: String,
        cancelled_at: DateTime<Utc>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SharingEventV1 {
    pub metadata: SharingEventMetadataV1,
    pub fact: SharingEventFactV1,
}
