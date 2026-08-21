//! Task-oriented Sharing commands and canonical request hashing.

use crate::contexts::sharing::domain::{
    BillSplitId, BillVersion, ContactId, ContactName, ContactVersion, Contribution, Participant,
    SettlementEvidence, SettlementId, SettlementVersion, ShareRequest,
};
use crate::shared_kernel::{CorrelationId, IdempotencyKey, Money, UserId};
use chrono::{DateTime, Utc};
use serde::Serialize;
use sha2::{Digest, Sha256};

#[derive(Clone, Debug)]
pub struct CommandMetadata {
    pub user_id: UserId,
    pub idempotency_key: IdempotencyKey,
    pub request_hash: [u8; 32],
    pub correlation_id: CorrelationId,
    pub occurred_at: DateTime<Utc>,
}

pub fn canonical_request_hash<T: Serialize>(value: &T) -> Result<[u8; 32], serde_json::Error> {
    let bytes = serde_json::to_vec(value)?;
    Ok(Sha256::digest(bytes).into())
}

#[derive(Clone, Debug)]
pub struct CreateContact {
    pub metadata: CommandMetadata,
    pub name: ContactName,
    pub note: Option<String>,
}
#[derive(Clone, Debug)]
pub struct UpdateContact {
    pub metadata: CommandMetadata,
    pub contact_id: ContactId,
    pub name: ContactName,
    pub note: Option<String>,
    pub expected_version: ContactVersion,
}
#[derive(Clone, Debug)]
pub struct ArchiveContact {
    pub metadata: CommandMetadata,
    pub contact_id: ContactId,
    pub expected_version: ContactVersion,
}

#[derive(Clone, Debug)]
pub struct BillDraft {
    pub title: String,
    pub occurred_at: DateTime<Utc>,
    pub total: Money,
    pub minor_unit_scale: u32,
    pub contributions: Vec<Contribution>,
    pub shares: ShareRequest,
}

#[derive(Clone, Debug)]
pub struct CreateBillSplit {
    pub metadata: CommandMetadata,
    pub draft: BillDraft,
}
#[derive(Clone, Debug)]
pub struct ReviseBillSplit {
    pub metadata: CommandMetadata,
    pub bill_id: BillSplitId,
    pub expected_version: BillVersion,
    pub draft: BillDraft,
}
#[derive(Clone, Debug)]
pub struct CancelBillSplit {
    pub metadata: CommandMetadata,
    pub bill_id: BillSplitId,
    pub expected_version: BillVersion,
    pub reason: String,
}

#[derive(Clone, Debug)]
pub struct CreateSettlement {
    pub metadata: CommandMetadata,
    pub bill_id: BillSplitId,
    pub expected_version: BillVersion,
    pub debtor: Participant,
    pub creditor: Participant,
    pub amount: Money,
    pub evidence: SettlementEvidence,
}

#[derive(Clone, Debug)]
pub struct ReverseSettlement {
    pub metadata: CommandMetadata,
    pub bill_id: BillSplitId,
    pub settlement_id: SettlementId,
    pub expected_version: SettlementVersion,
    pub reason: String,
}

#[derive(Clone, Debug)]
pub struct CompleteBillAccounting {
    pub user_id: UserId,
    pub bill_id: BillSplitId,
    pub revision: u32,
    pub expected_version: BillVersion,
    pub journal_id: Option<uuid::Uuid>,
    pub correlation_id: CorrelationId,
    pub occurred_at: DateTime<Utc>,
}

#[derive(Clone, Debug)]
pub struct CompleteBillCancellation {
    pub user_id: UserId,
    pub bill_id: BillSplitId,
    pub expected_version: BillVersion,
    pub reversal_journal_id: Option<uuid::Uuid>,
    pub correlation_id: CorrelationId,
    pub occurred_at: DateTime<Utc>,
}

#[derive(Clone, Debug)]
pub struct CompleteSettlementAccounting {
    pub user_id: UserId,
    pub bill_id: BillSplitId,
    pub settlement_id: SettlementId,
    pub expected_version: SettlementVersion,
    pub journal_id: Option<uuid::Uuid>,
    pub correlation_id: CorrelationId,
    pub occurred_at: DateTime<Utc>,
}

#[derive(Clone, Debug)]
pub struct CompleteSettlementReversal {
    pub user_id: UserId,
    pub bill_id: BillSplitId,
    pub settlement_id: SettlementId,
    pub reversal_journal_id: Option<uuid::Uuid>,
    pub correlation_id: CorrelationId,
    pub occurred_at: DateTime<Utc>,
}
