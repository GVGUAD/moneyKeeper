//! Stable contracts published by Mail.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use super::application::ports::GmailOAuth;
use super::infrastructure::PgMailStore;
use crate::shared_kernel::{CurrencyCode, Money, UserId};

pub use super::domain::{
    ConnectionState, ConnectionVersion, GmailConnectionId, MailError, SourceMessageId,
};
crate::define_uuid_id!(#[doc = "Identifies normalized receipt evidence."] pub ReceiptEvidenceId);

pub const CONTEXT_NAME: &str = "mail";
pub const RECEIPT_EVIDENCE_RECORDED_V1: &str = "mail.receipt-evidence-recorded.v1";

#[derive(Clone)]
pub struct MailFacade {
    pub(crate) store: PgMailStore,
    pub(crate) oauth: Arc<dyn GmailOAuth>,
}
impl MailFacade {
    pub(crate) fn new(store: PgMailStore, oauth: Arc<dyn GmailOAuth>) -> Self {
        Self { store, oauth }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReceiptEvidenceKind {
    Renewal,
    OneTime,
    Refund,
    Cancellation,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ReceiptEvidenceRecordedV1 {
    pub evidence_id: ReceiptEvidenceId,
    pub user_id: UserId,
    pub source_message_id: SourceMessageId,
    pub merchant: String,
    pub kind: ReceiptEvidenceKind,
    pub money: Option<Money>,
    pub charged_at: Option<DateTime<Utc>>,
    pub parser_name: String,
    pub parser_version: u32,
    pub provenance_digest: [u8; 32],
    pub recorded_at: DateTime<Utc>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectionView {
    pub id: GmailConnectionId,
    pub state: ConnectionState,
    pub version: ConnectionVersion,
    pub credential_generation: u64,
    pub sync_generation: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartOauth {
    pub replacement_connection_id: Option<GmailConnectionId>,
    pub expected_version: Option<ConnectionVersion>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReceiptEvidenceView {
    pub id: ReceiptEvidenceId,
    pub merchant: String,
    pub kind: ReceiptEvidenceKind,
    pub amount: Option<String>,
    pub currency: Option<CurrencyCode>,
    pub charged_at: Option<DateTime<Utc>>,
}
