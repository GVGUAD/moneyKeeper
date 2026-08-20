//! Banking command/query facade with private adapters.

use std::sync::Arc;

use super::ports::{CredentialCipher, ProviderClient};
use super::{
    BindExistingResource, ConnectProvider, ConnectionResult, CreateAndMapResource,
    DeactivateResourceMapping, ProviderConnectionView, ReplaceProviderCredential,
    ResourceMappingResult,
    IntakeProviderEvent, ProviderEventReceipt,
    ProviderImportOutcome, ProviderImportWork,
};
use crate::contexts::banking::domain::BankingError;
use crate::contexts::banking::infrastructure::PgBankingStore;
use crate::shared_kernel::UserId;

#[derive(Clone)]
pub struct BankingFacade {
    pub(crate) store: PgBankingStore,
    pub(crate) cipher: Arc<dyn CredentialCipher>,
    pub(crate) provider: Arc<dyn ProviderClient>,
    pub(crate) ledger: Option<crate::contexts::ledger::public::LedgerFacade>,
    pub(crate) currencies: crate::contexts::reference_data::public::CurrencyCatalogFacade,
}

impl BankingFacade {
    pub(crate) fn new(store: PgBankingStore, cipher: Arc<dyn CredentialCipher>, provider: Arc<dyn ProviderClient>, ledger: Option<crate::contexts::ledger::public::LedgerFacade>, currencies: crate::contexts::reference_data::public::CurrencyCatalogFacade) -> Self { Self { store, cipher, provider, ledger, currencies } }

    pub async fn connect_provider(&self, command: ConnectProvider) -> Result<ConnectionResult, BankingError> {
        self.store.connect(command, self.cipher.as_ref()).await
    }

    pub async fn replace_provider_credential(&self, command: ReplaceProviderCredential) -> Result<ConnectionResult, BankingError> {
        self.store.replace_credential(command, self.cipher.as_ref()).await
    }

    pub async fn validate_and_discover(&self, user_id: UserId, connection_id: super::super::domain::ProviderConnectionId) -> Result<Vec<super::super::infrastructure::NormalizedResource>, BankingError> {
        use crate::contexts::reference_data::public::CurrencyCatalog;
        let currencies = self.currencies.list_enabled().await.map_err(|_| BankingError::InvalidValue("currency catalog unavailable"))?
            .into_iter().filter_map(|definition| definition.numeric_code.and_then(|numeric| numeric.parse::<u16>().ok()).map(|numeric| (numeric, (definition.code, definition.minor_unit)))).collect();
        self.store.validate_and_discover(user_id, connection_id, self.cipher.as_ref(), self.provider.as_ref(), &currencies).await
    }

    pub async fn list_connections(&self, user_id: UserId) -> Result<Vec<ProviderConnectionView>, BankingError> { self.store.list_connections(user_id).await }

    pub async fn bind_existing_resource(&self, command: BindExistingResource) -> Result<ResourceMappingResult, BankingError> {
        let ledger = self.ledger.as_ref().ok_or(BankingError::InvalidState)?;
        let resource = self.store.resource_binding(command.user_id, command.resource_id).await?;
        if resource.version != command.expected_resource_version { return Err(BankingError::VersionConflict); }
        let (kind, nature) = resource.expected_ledger_account()?;
        let outcome = ledger.validate_provider_account_binding(crate::contexts::ledger::public::ValidateProviderAccountBinding {
            user_id: command.user_id, account_id: command.ledger_account_id,
            currency: resource.currency.clone(), kind, nature,
        }).await.map_err(|_| BankingError::IncompatibleMapping)?;
        if !matches!(outcome, crate::contexts::ledger::public::ProviderAccountBindingResult::Accepted(_)) { return Err(BankingError::IncompatibleMapping); }
        self.store.commit_mapping(command).await
    }

    pub async fn create_and_map_resource(&self, command: CreateAndMapResource) -> Result<ResourceMappingResult, BankingError> {
        let ledger = self.ledger.as_ref().ok_or(BankingError::InvalidState)?;
        let pending = self.store.ensure_pending_mapping(&command).await?;
        if pending.mapping.ledger_account_id.is_some() && pending.mapping.state == "active" { return Ok(ResourceMappingResult { replayed: true, ..pending }); }
        let resource = self.store.resource_binding(command.user_id, command.resource_id).await?;
        let (kind, nature) = resource.expected_ledger_account()?;
        let opened = ledger.open_provider_observed_account(crate::contexts::ledger::public::OpenProviderObservedAccount {
            user_id: command.user_id, name: command.account_name.clone(), currency: resource.currency,
            kind, nature,
            source: crate::contexts::ledger::public::SourceReference::new(
                "banking",
                command.resource_id.to_string(),
                format!("mapping:{}", pending.mapping.mapping_version),
            ).map_err(|_| BankingError::InvalidValue("invalid mapping source"))?,
            idempotency_key: crate::shared_kernel::IdempotencyKey::new(format!("banking-resource-account:{}:{}", command.resource_id, pending.mapping.mapping_version)).map_err(|_| BankingError::InvalidValue("invalid mapping idempotency key"))?,
            correlation_id: command.correlation_id, causation_id: None, occurred_at: command.requested_at,
        }).await.map_err(|_| BankingError::IncompatibleMapping)?;
        self.store.complete_pending_mapping(command.user_id, command.resource_id, pending.mapping.id, pending.mapping.mapping_version, opened.account.id, command.requested_at).await
    }

    pub async fn deactivate_resource_mapping(&self, command: DeactivateResourceMapping) -> Result<ResourceMappingResult, BankingError> {
        self.store.deactivate_mapping(command).await
    }

    pub async fn intake_provider_event(&self, command: IntakeProviderEvent) -> Result<ProviderEventReceipt, BankingError> {
        self.store.intake_provider_event(command).await
    }

    pub async fn claim_provider_import(&self, user_id: UserId, provider_event_id: super::super::domain::ProviderEventId) -> Result<Option<ProviderImportWork>, BankingError> {
        self.store.claim_provider_import(user_id,provider_event_id).await
    }

    pub async fn complete_provider_import(&self, outcome: ProviderImportOutcome) -> Result<ProviderImportOutcome, BankingError> {
        self.store.complete_provider_import(outcome).await
    }
}
