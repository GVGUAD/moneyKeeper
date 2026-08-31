//! Application repository ports implemented by the PostgreSQL adapter.

use async_trait::async_trait;
use chrono::{DateTime, Utc};

use super::PgBankingStore;
use crate::{
    contexts::{
        banking::{application::*, domain::*},
        ledger::public::LedgerAccountId,
    },
    shared_kernel::UserId,
};

#[async_trait]
impl ConnectionRepository for PgBankingStore {
    async fn connect(
        &self,
        command: ConnectProvider,
        cipher: &dyn CredentialCipher,
    ) -> Result<ConnectionResult, BankingError> {
        PgBankingStore::connect(self, command, cipher).await
    }
    async fn replace_credential(
        &self,
        command: ReplaceProviderCredential,
        cipher: &dyn CredentialCipher,
    ) -> Result<ConnectionResult, BankingError> {
        PgBankingStore::replace_credential(self, command, cipher).await
    }
    async fn list_connections(
        &self,
        user_id: UserId,
    ) -> Result<Vec<ProviderConnectionView>, BankingError> {
        PgBankingStore::list_connections(self, user_id).await
    }
    async fn get_connection(
        &self,
        user_id: UserId,
        id: ProviderConnectionId,
    ) -> Result<ProviderConnectionView, BankingError> {
        PgBankingStore::get_connection(self, user_id, id).await
    }
    async fn disconnect(
        &self,
        user_id: UserId,
        id: ProviderConnectionId,
        expected: ConnectionVersion,
        now: DateTime<Utc>,
    ) -> Result<ProviderConnectionView, BankingError> {
        PgBankingStore::disconnect(self, user_id, id, expected, now).await
    }
}

#[async_trait]
impl ResourceRepository for PgBankingStore {
    async fn validate_and_discover(
        &self,
        user_id: UserId,
        connection_id: ProviderConnectionId,
        cipher: &dyn CredentialCipher,
        provider: &dyn ProviderClient,
        currencies: &ProviderCurrencyMap,
    ) -> Result<Vec<NormalizedResource>, BankingError> {
        PgBankingStore::validate_and_discover(
            self,
            user_id,
            connection_id,
            cipher,
            provider,
            currencies,
        )
        .await
    }
    async fn list_resources(
        &self,
        user_id: UserId,
        connection_id: ProviderConnectionId,
    ) -> Result<Vec<ExternalResourceView>, BankingError> {
        PgBankingStore::list_resources(self, user_id, connection_id).await
    }
    async fn resource_binding(
        &self,
        user_id: UserId,
        resource_id: ExternalResourceId,
    ) -> Result<ResourceBinding, BankingError> {
        PgBankingStore::resource_binding(self, user_id, resource_id).await
    }
    async fn require_resource_connection(
        &self,
        user_id: UserId,
        connection_id: ProviderConnectionId,
        resource_id: ExternalResourceId,
    ) -> Result<(), BankingError> {
        PgBankingStore::require_resource_connection(self, user_id, connection_id, resource_id).await
    }
    async fn resource_for_mapping(
        &self,
        user_id: UserId,
        connection_id: ProviderConnectionId,
        mapping_id: ResourceMappingId,
    ) -> Result<ExternalResourceId, BankingError> {
        PgBankingStore::resource_for_mapping(self, user_id, connection_id, mapping_id).await
    }
    async fn resource_id_by_external(
        &self,
        user_id: UserId,
        connection_id: ProviderConnectionId,
        external_id: &str,
    ) -> Result<ExternalResourceId, BankingError> {
        PgBankingStore::resource_id_by_external(self, user_id, connection_id, external_id).await
    }
    async fn commit_mapping(
        &self,
        command: BindExistingResource,
    ) -> Result<ResourceMappingResult, BankingError> {
        PgBankingStore::commit_mapping(self, command).await
    }
    async fn ensure_pending_mapping(
        &self,
        command: &CreateAndMapResource,
    ) -> Result<ResourceMappingResult, BankingError> {
        PgBankingStore::ensure_pending_mapping(self, command).await
    }
    async fn complete_pending_mapping(
        &self,
        user_id: UserId,
        resource_id: ExternalResourceId,
        mapping_id: ResourceMappingId,
        mapping_version: i64,
        ledger_account_id: LedgerAccountId,
        now: DateTime<Utc>,
    ) -> Result<ResourceMappingResult, BankingError> {
        PgBankingStore::complete_pending_mapping(
            self,
            user_id,
            resource_id,
            mapping_id,
            mapping_version,
            ledger_account_id,
            now,
        )
        .await
    }
    async fn deactivate_mapping(
        &self,
        command: DeactivateResourceMapping,
    ) -> Result<ResourceMappingResult, BankingError> {
        PgBankingStore::deactivate_mapping(self, command).await
    }
    async fn provider_account_summary(
        &self,
        user_id: UserId,
        account_id: LedgerAccountId,
    ) -> Result<ProviderAccountSummary, BankingError> {
        PgBankingStore::provider_account_summary(self, user_id, account_id).await
    }
}

#[async_trait]
impl ProviderEventRepository for PgBankingStore {
    async fn get_provider_event(
        &self,
        user_id: UserId,
        id: ProviderEventId,
    ) -> Result<ProviderEventView, BankingError> {
        PgBankingStore::get_provider_event(self, user_id, id).await
    }
    async fn get_accounting_process(
        &self,
        user_id: UserId,
        id: uuid::Uuid,
    ) -> Result<AccountingProcessView, BankingError> {
        PgBankingStore::get_accounting_process(self, user_id, id).await
    }
    async fn intake_provider_event(
        &self,
        command: IntakeProviderEvent,
    ) -> Result<ProviderEventReceipt, BankingError> {
        PgBankingStore::intake_provider_event(self, command).await
    }
    async fn claim_provider_import(
        &self,
        user_id: UserId,
        id: ProviderEventId,
        holder: &str,
        now: DateTime<Utc>,
        lease_seconds: i64,
    ) -> Result<Option<ProviderImportWork>, BankingError> {
        PgBankingStore::claim_provider_import(self, user_id, id, holder, now, lease_seconds).await
    }
    async fn next_provider_import_candidate(
        &self,
    ) -> Result<Option<(UserId, ProviderEventId)>, BankingError> {
        PgBankingStore::next_provider_import_candidate(self).await
    }
    async fn complete_provider_import(
        &self,
        outcome: ProviderImportOutcome,
    ) -> Result<ProviderImportOutcome, BankingError> {
        PgBankingStore::complete_provider_import(self, outcome).await
    }
    async fn list_provider_event_conflicts(
        &self,
        user_id: UserId,
        connection_id: ProviderConnectionId,
    ) -> Result<Vec<ProviderEventConflictView>, BankingError> {
        PgBankingStore::list_provider_event_conflicts(self, user_id, connection_id).await
    }
}

#[async_trait]
impl SyncJobRepository for PgBankingStore {
    async fn request_sync_job(&self, command: RequestSyncJob) -> Result<SyncJobView, BankingError> {
        PgBankingStore::request_sync_job(self, command).await
    }
    async fn claim_due_sync_job(
        &self,
        holder: &str,
        now: DateTime<Utc>,
        lease_seconds: i64,
    ) -> Result<Option<SyncJobView>, BankingError> {
        PgBankingStore::claim_due_sync_job(self, holder, now, lease_seconds).await
    }
    async fn begin_sync_page(&self, command: BeginSyncPage) -> Result<SyncPageView, BankingError> {
        PgBankingStore::begin_sync_page(self, command).await
    }
    async fn complete_sync_page(
        &self,
        command: CompleteSyncPage,
    ) -> Result<SyncJobView, BankingError> {
        PgBankingStore::complete_sync_page(self, command).await
    }
    async fn get_sync_job(
        &self,
        user_id: UserId,
        id: SyncJobId,
    ) -> Result<SyncJobView, BankingError> {
        PgBankingStore::get_sync_job(self, user_id, id).await
    }
    async fn list_sync_pages(
        &self,
        user_id: UserId,
        id: SyncJobId,
    ) -> Result<Vec<SyncPageView>, BankingError> {
        PgBankingStore::list_sync_pages(self, user_id, id).await
    }
}

#[async_trait]
impl ObservationRepository for PgBankingStore {
    async fn get_balance_observation(
        &self,
        user_id: UserId,
        id: BalanceObservationId,
    ) -> Result<BalanceObservationView, BankingError> {
        PgBankingStore::get_balance_observation(self, user_id, id).await
    }
    async fn record_balance_observation(
        &self,
        command: RecordBalanceObservation,
    ) -> Result<BalanceObservationView, BankingError> {
        PgBankingStore::record_balance_observation(self, command).await
    }
    async fn claim_balance_observation(
        &self,
        user_id: UserId,
        id: BalanceObservationId,
        holder: &str,
        now: DateTime<Utc>,
        lease_seconds: i64,
    ) -> Result<Option<BalanceObservationDeliveryWork>, BankingError> {
        PgBankingStore::claim_balance_observation(self, user_id, id, holder, now, lease_seconds)
            .await
    }
    async fn next_balance_observation_candidate(
        &self,
    ) -> Result<Option<(UserId, BalanceObservationId)>, BankingError> {
        PgBankingStore::next_balance_observation_candidate(self).await
    }
    async fn complete_balance_observation(
        &self,
        outcome: BalanceObservationDeliveryOutcome,
    ) -> Result<BalanceObservationDeliveryOutcome, BankingError> {
        PgBankingStore::complete_balance_observation(self, outcome).await
    }
}

#[async_trait]
impl WebhookRepository for PgBankingStore {
    async fn rotate_webhook(
        &self,
        command: RotateWebhookCredential,
        credential: WebhookCredential,
        digest: [u8; 32],
        cipher: &dyn CredentialCipher,
    ) -> Result<WebhookRotationResult, BankingError> {
        PgBankingStore::rotate_webhook(self, command, credential, digest, cipher).await
    }
    async fn validate_webhook_digest(
        &self,
        digest: &[u8; 32],
        secrets: &dyn WebhookSecrets,
    ) -> Result<bool, BankingError> {
        PgBankingStore::validate_webhook_digest(self, digest, secrets).await
    }
    async fn receive_webhook(
        &self,
        digest: &[u8; 32],
        body: &[u8],
        secrets: &dyn WebhookSecrets,
        cipher: &dyn CredentialCipher,
    ) -> Result<WebhookReceiptOutcome, BankingError> {
        PgBankingStore::receive_webhook(self, digest, body, secrets, cipher).await
    }
    async fn webhook_registration_work(
        &self,
        user_id: UserId,
        connection_id: ProviderConnectionId,
    ) -> Result<WebhookRegistrationWork, BankingError> {
        PgBankingStore::webhook_registration_work(self, user_id, connection_id).await
    }
    async fn complete_webhook_registration(
        &self,
        user_id: UserId,
        connection_id: ProviderConnectionId,
        version: i64,
        success: bool,
    ) -> Result<(), BankingError> {
        PgBankingStore::complete_webhook_registration(
            self,
            user_id,
            connection_id,
            version,
            success,
        )
        .await
    }
}
