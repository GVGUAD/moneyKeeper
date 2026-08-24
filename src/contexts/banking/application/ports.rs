//! Provider-neutral credential and remote-provider boundaries.

use std::{collections::BTreeMap, fmt};

use async_trait::async_trait;
use zeroize::Zeroize;

use crate::shared_kernel::{CurrencyCode, Money, UserId};

use super::super::domain::{
    BalanceObservationId, ConnectionVersion, ExternalResourceId, FundingModel,
    ProviderConnectionId, ProviderEventId, ResourceKind, ResourceMappingId, SyncJobId,
};
use super::super::domain::{BankingError, CredentialEnvelope};
use super::{
    AccountingProcessView, BalanceObservationDeliveryOutcome, BalanceObservationDeliveryWork,
    BalanceObservationView, BeginSyncPage, BindExistingResource, CompleteSyncPage, ConnectProvider,
    ConnectionResult, CreateAndMapResource, DeactivateResourceMapping, ExternalResourceView,
    IntakeProviderEvent, ProviderAccountSummary, ProviderConnectionView, ProviderEventReceipt,
    ProviderEventView, ProviderImportOutcome, ProviderImportWork, RecordBalanceObservation,
    ReplaceProviderCredential, RequestSyncJob, ResourceMappingResult, RotateWebhookCredential,
    SyncJobView, SyncPageView, WebhookReceiptOutcome, WebhookRotationResult,
};
use crate::contexts::ledger::public::{AccountKind, AccountNature, LedgerAccountId};
use chrono::{DateTime, Utc};

/// Provider-neutral account resource produced by an anti-corruption adapter.
#[derive(Clone)]
pub struct NormalizedResource {
    pub external_resource_id: String,
    pub kind: super::super::domain::ResourceKind,
    pub funding_model: super::super::domain::FundingModel,
    pub currency: CurrencyCode,
    pub masked_label: String,
    pub provider_balance: Money,
    pub credit_limit: Option<Money>,
}

impl fmt::Debug for NormalizedResource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NormalizedResource")
            .field("external_resource_id", &"[REDACTED]")
            .field("kind", &self.kind)
            .field("funding_model", &self.funding_model)
            .field("currency", &self.currency)
            .field("masked_label", &"[REDACTED]")
            .field("provider_balance", &self.provider_balance)
            .field("credit_limit", &self.credit_limit)
            .finish()
    }
}

#[derive(Clone, Debug)]
pub struct NormalizedSnapshot {
    pub resources: Vec<NormalizedResource>,
}

/// Secret callback credential returned only at rotation time.
pub struct WebhookCredential(String);

impl WebhookCredential {
    pub fn new(value: impl Into<String>) -> Result<Self, BankingError> {
        let value = value.into();
        if value.len() < 43
            || value.len() > 100
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            return Err(BankingError::InvalidValue("invalid webhook credential"));
        }
        Ok(Self(value))
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for WebhookCredential {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("WebhookCredential([REDACTED])")
    }
}

impl Drop for WebhookCredential {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

pub(crate) trait WebhookSecrets: Send + Sync {
    fn generate(&self) -> WebhookCredential;
    fn digest(&self, credential: &WebhookCredential) -> [u8; 32];
    fn verify_digest(&self, actual: &[u8], expected: &[u8]) -> bool;
}

pub(crate) struct WebhookRegistrationWork {
    pub(crate) provider: String,
    pub(crate) credential_generation: i64,
    pub(crate) webhook_version: i64,
    pub(crate) provider_envelope: CredentialEnvelope,
    pub(crate) webhook_envelope: CredentialEnvelope,
}

pub(crate) struct ResourceBinding {
    pub(crate) kind: ResourceKind,
    pub(crate) funding_model: FundingModel,
    pub(crate) currency: CurrencyCode,
    pub(crate) version: i64,
}

impl ResourceBinding {
    pub(crate) fn expected_ledger_account(
        &self,
    ) -> Result<(AccountKind, AccountNature), BankingError> {
        match (self.kind, self.funding_model) {
            (ResourceKind::Card, FundingModel::OwnFunds) => {
                Ok((AccountKind::DebitCard, AccountNature::Asset))
            }
            (ResourceKind::CurrentAccount, FundingModel::OwnFunds) => {
                Ok((AccountKind::Current, AccountNature::Asset))
            }
            (ResourceKind::Jar, FundingModel::OwnFunds) => {
                Ok((AccountKind::Jar, AccountNature::Asset))
            }
            (ResourceKind::Card, FundingModel::RevolvingCredit) => {
                Ok((AccountKind::CreditCard, AccountNature::Liability))
            }
            (ResourceKind::SecurityPortfolio, _) => Err(BankingError::RouteToPortfolio),
            _ => Err(BankingError::IncompatibleMapping),
        }
    }
}

#[async_trait]
pub(crate) trait ConnectionRepository: Send + Sync {
    async fn connect(
        &self,
        command: ConnectProvider,
        cipher: &dyn CredentialCipher,
    ) -> Result<ConnectionResult, BankingError>;
    async fn replace_credential(
        &self,
        command: ReplaceProviderCredential,
        cipher: &dyn CredentialCipher,
    ) -> Result<ConnectionResult, BankingError>;
    async fn list_connections(
        &self,
        user_id: UserId,
    ) -> Result<Vec<ProviderConnectionView>, BankingError>;
    async fn get_connection(
        &self,
        user_id: UserId,
        id: ProviderConnectionId,
    ) -> Result<ProviderConnectionView, BankingError>;
    async fn disconnect(
        &self,
        user_id: UserId,
        id: ProviderConnectionId,
        expected: ConnectionVersion,
        now: DateTime<Utc>,
    ) -> Result<ProviderConnectionView, BankingError>;
}

#[async_trait]
pub(crate) trait ResourceRepository: Send + Sync {
    async fn validate_and_discover(
        &self,
        user_id: UserId,
        connection_id: ProviderConnectionId,
        cipher: &dyn CredentialCipher,
        provider: &dyn ProviderClient,
        currencies: &BTreeMap<u16, (CurrencyCode, u8)>,
    ) -> Result<Vec<NormalizedResource>, BankingError>;
    async fn list_resources(
        &self,
        user_id: UserId,
        connection_id: ProviderConnectionId,
    ) -> Result<Vec<ExternalResourceView>, BankingError>;
    async fn resource_binding(
        &self,
        user_id: UserId,
        resource_id: ExternalResourceId,
    ) -> Result<ResourceBinding, BankingError>;
    async fn require_resource_connection(
        &self,
        user_id: UserId,
        connection_id: ProviderConnectionId,
        resource_id: ExternalResourceId,
    ) -> Result<(), BankingError>;
    async fn resource_for_mapping(
        &self,
        user_id: UserId,
        connection_id: ProviderConnectionId,
        mapping_id: ResourceMappingId,
    ) -> Result<ExternalResourceId, BankingError>;
    async fn resource_id_by_external(
        &self,
        user_id: UserId,
        connection_id: ProviderConnectionId,
        external_id: &str,
    ) -> Result<ExternalResourceId, BankingError>;
    async fn commit_mapping(
        &self,
        command: BindExistingResource,
    ) -> Result<ResourceMappingResult, BankingError>;
    async fn ensure_pending_mapping(
        &self,
        command: &CreateAndMapResource,
    ) -> Result<ResourceMappingResult, BankingError>;
    async fn complete_pending_mapping(
        &self,
        user_id: UserId,
        resource_id: ExternalResourceId,
        mapping_id: ResourceMappingId,
        mapping_version: i64,
        ledger_account_id: LedgerAccountId,
        now: DateTime<Utc>,
    ) -> Result<ResourceMappingResult, BankingError>;
    async fn deactivate_mapping(
        &self,
        command: DeactivateResourceMapping,
    ) -> Result<ResourceMappingResult, BankingError>;
    async fn provider_account_summary(
        &self,
        user_id: UserId,
        account_id: LedgerAccountId,
    ) -> Result<ProviderAccountSummary, BankingError>;
}

#[async_trait]
pub(crate) trait ProviderEventRepository: Send + Sync {
    async fn get_provider_event(
        &self,
        user_id: UserId,
        id: ProviderEventId,
    ) -> Result<ProviderEventView, BankingError>;
    async fn get_accounting_process(
        &self,
        user_id: UserId,
        id: uuid::Uuid,
    ) -> Result<AccountingProcessView, BankingError>;
    async fn intake_provider_event(
        &self,
        command: IntakeProviderEvent,
    ) -> Result<ProviderEventReceipt, BankingError>;
    async fn claim_provider_import(
        &self,
        user_id: UserId,
        id: ProviderEventId,
    ) -> Result<Option<ProviderImportWork>, BankingError>;
    async fn next_provider_import_candidate(
        &self,
    ) -> Result<Option<(UserId, ProviderEventId)>, BankingError>;
    async fn complete_provider_import(
        &self,
        outcome: ProviderImportOutcome,
    ) -> Result<ProviderImportOutcome, BankingError>;
}

#[async_trait]
pub(crate) trait SyncJobRepository: Send + Sync {
    async fn request_sync_job(&self, command: RequestSyncJob) -> Result<SyncJobView, BankingError>;
    async fn claim_due_sync_job(
        &self,
        holder: &str,
        now: DateTime<Utc>,
        lease_seconds: i64,
    ) -> Result<Option<SyncJobView>, BankingError>;
    async fn begin_sync_page(&self, command: BeginSyncPage) -> Result<SyncPageView, BankingError>;
    async fn complete_sync_page(
        &self,
        command: CompleteSyncPage,
    ) -> Result<SyncJobView, BankingError>;
    async fn get_sync_job(
        &self,
        user_id: UserId,
        id: SyncJobId,
    ) -> Result<SyncJobView, BankingError>;
}

#[async_trait]
pub(crate) trait ObservationRepository: Send + Sync {
    async fn get_balance_observation(
        &self,
        user_id: UserId,
        id: BalanceObservationId,
    ) -> Result<BalanceObservationView, BankingError>;
    async fn record_balance_observation(
        &self,
        command: RecordBalanceObservation,
    ) -> Result<BalanceObservationView, BankingError>;
    async fn claim_balance_observation(
        &self,
        user_id: UserId,
        id: BalanceObservationId,
    ) -> Result<Option<BalanceObservationDeliveryWork>, BankingError>;
    async fn next_balance_observation_candidate(
        &self,
    ) -> Result<Option<(UserId, BalanceObservationId)>, BankingError>;
    async fn complete_balance_observation(
        &self,
        outcome: BalanceObservationDeliveryOutcome,
    ) -> Result<BalanceObservationDeliveryOutcome, BankingError>;
}

#[async_trait]
pub(crate) trait WebhookRepository: Send + Sync {
    async fn rotate_webhook(
        &self,
        command: RotateWebhookCredential,
        credential: WebhookCredential,
        digest: [u8; 32],
        cipher: &dyn CredentialCipher,
    ) -> Result<WebhookRotationResult, BankingError>;
    async fn validate_webhook_digest(
        &self,
        digest: &[u8; 32],
        secrets: &dyn WebhookSecrets,
    ) -> Result<bool, BankingError>;
    async fn receive_webhook(
        &self,
        digest: &[u8; 32],
        body: &[u8],
        secrets: &dyn WebhookSecrets,
    ) -> Result<WebhookReceiptOutcome, BankingError>;
    async fn webhook_registration_work(
        &self,
        user_id: UserId,
        connection_id: ProviderConnectionId,
    ) -> Result<WebhookRegistrationWork, BankingError>;
    async fn complete_webhook_registration(
        &self,
        user_id: UserId,
        connection_id: ProviderConnectionId,
        version: i64,
        success: bool,
    ) -> Result<(), BankingError>;
}

#[derive(Clone)]
pub struct ProviderCredential(String);

impl ProviderCredential {
    pub fn new(value: impl Into<String>) -> Result<Self, BankingError> {
        let value = value.into();
        if value.is_empty() || value.len() > 500 || value.chars().any(char::is_control) {
            return Err(BankingError::InvalidValue("invalid provider credential"));
        }
        Ok(Self(value))
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ProviderCredential {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ProviderCredential([REDACTED])")
    }
}

impl Drop for ProviderCredential {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CredentialBinding {
    user_id: UserId,
    connection_id: uuid::Uuid,
    provider: String,
    generation: i64,
    slot: String,
}

impl CredentialBinding {
    pub fn new(
        user_id: UserId,
        connection_id: uuid::Uuid,
        provider: impl Into<String>,
        generation: i64,
        slot: impl Into<String>,
    ) -> Result<Self, BankingError> {
        let provider = provider.into();
        let slot = slot.into();
        if provider.is_empty()
            || provider.len() > 100
            || generation < 1
            || !matches!(
                slot.as_str(),
                "active" | "pending" | "webhook" | "provenance"
            )
        {
            return Err(BankingError::InvalidValue("invalid credential binding"));
        }
        Ok(Self {
            user_id,
            connection_id,
            provider,
            generation,
            slot,
        })
    }

    pub(crate) fn associated_data(&self) -> Vec<u8> {
        format!(
            "banking|{}|{}|{}|{}|{}",
            self.user_id, self.connection_id, self.provider, self.generation, self.slot
        )
        .into_bytes()
    }
}

pub trait CredentialCipher: Send + Sync {
    fn encrypt(
        &self,
        credential: &ProviderCredential,
        binding: &CredentialBinding,
    ) -> Result<CredentialEnvelope, BankingError>;
    fn decrypt(
        &self,
        envelope: &CredentialEnvelope,
        binding: &CredentialBinding,
    ) -> Result<ProviderCredential, BankingError>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderFailureClass {
    RateLimited,
    Transient,
    NeedsReauth,
    Terminal,
}

#[derive(Debug, thiserror::Error)]
pub enum ProviderFailure {
    #[error("provider request failed ({class:?}); sensitive response omitted")]
    Classified { class: ProviderFailureClass },
    #[error("provider response could not be normalized")]
    InvalidResponse,
}

#[async_trait]
pub trait ProviderClient: Send + Sync {
    async fn client_info(&self, credential: &ProviderCredential)
    -> Result<String, ProviderFailure>;

    async fn register_webhook(
        &self,
        _credential: &ProviderCredential,
        _callback_url: &str,
    ) -> Result<(), ProviderFailure> {
        Err(ProviderFailure::Classified {
            class: ProviderFailureClass::Terminal,
        })
    }
}
