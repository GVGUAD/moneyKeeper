use serde::{Deserialize, Serialize};
#[derive(Deserialize, Serialize)]
pub(crate) struct UpdateSubscription {
    pub expected_version: u64,
    pub status: Option<String>,
    pub category_id: Option<uuid::Uuid>,
}
#[derive(Deserialize, Serialize)]
pub(crate) struct AllocationDto {
    pub journal_entry_id: uuid::Uuid,
    pub amount: String,
    pub currency: String,
}
#[derive(Deserialize, Serialize)]
pub(crate) struct MatchBody {
    pub expected_version: u64,
    pub allocations: Vec<AllocationDto>,
}
#[derive(Deserialize, Serialize)]
pub(crate) struct RejectionBody {
    pub expected_version: u64,
    pub reason: String,
}
#[derive(Deserialize, Serialize)]
pub(crate) struct UnmatchBody {
    pub expected_version: u64,
}
