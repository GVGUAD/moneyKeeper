use serde::{Deserialize, Serialize};
#[derive(Deserialize, Serialize)]
pub(crate) struct ExpectedVersion {
    pub expected_version: u64,
}
#[derive(Deserialize, Serialize)]
pub(crate) struct OauthStartBody {
    pub connection_id: Option<uuid::Uuid>,
    pub expected_version: Option<u64>,
}
#[derive(Deserialize)]
pub(crate) struct OauthCallbackQuery {
    pub state: String,
    pub code: String,
}
#[derive(Serialize)]
pub(crate) struct JobResponse {
    pub job_id: uuid::Uuid,
    pub status: &'static str,
    pub connection_version: u64,
}
