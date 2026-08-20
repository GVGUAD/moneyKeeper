use chrono::{DateTime, Utc};
use serde::Deserialize;
use uuid::Uuid;

#[derive(Deserialize)]
pub(crate) struct ConnectRequest {
    pub(crate) x_token: String,
}
#[derive(Deserialize)]
pub(crate) struct ExpectedVersionRequest {
    pub(crate) expected_version: i64,
}
#[derive(Deserialize)]
pub(crate) struct ReplaceCredentialRequest {
    pub(crate) x_token: String,
    pub(crate) expected_version: i64,
}
#[derive(Deserialize)]
pub(crate) struct MappingRequest {
    pub(crate) resource_id: Uuid,
    pub(crate) ledger_account_id: Option<Uuid>,
    pub(crate) account_name: Option<String>,
    pub(crate) expected_version: i64,
}
#[derive(Deserialize)]
pub(crate) struct MappingChangeRequest {
    pub(crate) resource_id: Uuid,
    pub(crate) expected_version: i64,
    pub(crate) reason: String,
    pub(crate) ledger_account_id: Option<Uuid>,
    pub(crate) account_name: Option<String>,
}
#[derive(Deserialize)]
pub(crate) struct SyncRequest {
    pub(crate) requested_from: DateTime<Utc>,
    pub(crate) requested_to: DateTime<Utc>,
    #[serde(default)]
    pub(crate) overlap_seconds: i32,
}
