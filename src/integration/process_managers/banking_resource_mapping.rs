//! Durable create-and-map orchestration through public context contracts.

use crate::contexts::banking::public::{
    BankingError, BankingFacade, CreateAndMapResource, ResourceMappingResult,
};

/// Completes a provider-account creation intent idempotently. Banking persists
/// the intent before its public Ledger dependency is invoked.
pub async fn create_provider_account_and_map(
    banking: &BankingFacade,
    command: CreateAndMapResource,
) -> Result<ResourceMappingResult, BankingError> {
    banking.create_and_map_resource(command).await
}
