use super::v2_db::{VerifiedV2Pool, initialize_v2};

/// Initializes and verifies the only executable Finance V2 database lineage.
pub async fn create_pool(database_url: &str) -> anyhow::Result<VerifiedV2Pool> {
    initialize_v2(database_url).await
}
