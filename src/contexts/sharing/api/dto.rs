//! Sharing request/response DTOs.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct MoneyDto {
    pub amount: String,
    pub currency: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", content = "contact_id", rename_all = "snake_case")]
pub enum ParticipantDto {
    CurrentUser,
    Contact(Uuid),
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct JournalAllocationDto {
    pub journal_id: Uuid,
    pub amount: MoneyDto,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ContributionEvidenceDto {
    External,
    Manual {
        account_id: Uuid,
    },
    ExistingJournals {
        allocations: Vec<JournalAllocationDto>,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ContributionDto {
    pub participant: ParticipantDto,
    pub amount: MoneyDto,
    pub evidence: ContributionEvidenceDto,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ExactShareDto {
    pub participant: ParticipantDto,
    pub amount: MoneyDto,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SharesDto {
    Exact { shares: Vec<ExactShareDto> },
    Equal { participants: Vec<ParticipantDto> },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ContactBody {
    pub display_name: String,
    pub note: Option<String>,
    pub expected_version: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ArchiveBody {
    pub expected_version: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct BillBody {
    pub title: String,
    pub occurred_at: DateTime<Utc>,
    pub total: MoneyDto,
    pub contributions: Vec<ContributionDto>,
    pub shares: SharesDto,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RevisionBody {
    pub expected_version: u64,
    #[serde(flatten)]
    pub bill: BillBody,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CancellationBody {
    pub expected_version: u64,
    pub reason: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SettlementEvidenceDto {
    External,
    Manual { account_id: Uuid },
    ExistingJournal { journal_id: Uuid },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SettlementBody {
    pub expected_version: u64,
    pub debtor: ParticipantDto,
    pub creditor: ParticipantDto,
    pub amount: MoneyDto,
    pub evidence: SettlementEvidenceDto,
    pub occurred_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ReversalBody {
    pub expected_version: u64,
    pub reason: String,
}
