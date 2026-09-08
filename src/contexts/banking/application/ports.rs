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
    IntakeProviderEvent, ProviderAccountSummary, ProviderClassificationEvidence,
    ProviderConnectionView, ProviderEventConflictView, ProviderEventReceipt, ProviderEventView,
    ProviderImportOutcome, ProviderImportWork, RecordBalanceObservation, ReplaceProviderCredential,
    RequestSyncJob, ResourceMappingResult, RotateWebhookCredential, SyncJobView, SyncPageView,
    WebhookReceiptOutcome, WebhookRotationResult,
};
use crate::contexts::ledger::public::{
    AccountKind, AccountNature, JournalEntryId, LedgerAccountId,
};
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

/// Currency metadata used at a provider boundary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderCurrency {
    pub code: CurrencyCode,
    pub minor_unit: u8,
    pub enabled: bool,
}

pub type ProviderCurrencyMap = BTreeMap<u16, ProviderCurrency>;

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
    pub(crate) user_id: UserId,
    pub(crate) connection_id: ProviderConnectionId,
    pub(crate) provider: String,
    pub(crate) credential_generation: i64,
    pub(crate) webhook_version: i64,
    pub(crate) provider_envelope: CredentialEnvelope,
    pub(crate) webhook_envelope: CredentialEnvelope,
    pub(crate) holder: String,
    pub(crate) fencing_token: i64,
    pub(crate) attempts: i32,
}

pub(crate) struct ValidationWork {
    pub(crate) user_id: UserId,
    pub(crate) connection_id: ProviderConnectionId,
    pub(crate) provider: String,
    pub(crate) generation: i64,
    pub(crate) replacement: bool,
    pub(crate) webhook_configured: bool,
    pub(crate) envelope: CredentialEnvelope,
    pub(crate) holder: String,
    pub(crate) fencing_token: i64,
    pub(crate) attempts: i32,
}

pub(crate) struct WebhookProvisioning {
    pub(crate) version: i64,
    pub(crate) envelope: CredentialEnvelope,
    pub(crate) digest: [u8; 32],
}

pub(crate) struct WebhookReceiptWork {
    pub(crate) receipt_id: uuid::Uuid,
    pub(crate) user_id: UserId,
    pub(crate) connection_id: ProviderConnectionId,
    pub(crate) provider: String,
    pub(crate) binding_generation: i64,
    pub(crate) envelope: CredentialEnvelope,
    pub(crate) holder: String,
    pub(crate) fencing_token: i64,
}

#[derive(Clone, Debug)]
pub(crate) struct NormalizedProviderEvent {
    pub(crate) external_event_id: String,
    pub(crate) state: super::super::domain::ProviderTransactionState,
    pub(crate) operation_money: Money,
    pub(crate) original_money: Option<Money>,
    pub(crate) description: String,
    pub(crate) merchant_mcc: Option<i32>,
    pub(crate) effective_at: DateTime<Utc>,
    pub(crate) running_balance: Option<Money>,
}

pub(crate) struct StatementWork {
    pub(crate) user_id: UserId,
    pub(crate) connection_id: ProviderConnectionId,
    pub(crate) sync_job_id: SyncJobId,
    pub(crate) resource_id: ExternalResourceId,
    pub(crate) external_resource_id: String,
    pub(crate) resource_currency: CurrencyCode,
    pub(crate) from: DateTime<Utc>,
    pub(crate) to: DateTime<Utc>,
    pub(crate) snapshot_to: DateTime<Utc>,
    pub(crate) balance_comparable: bool,
    pub(crate) provider: String,
    pub(crate) credential_generation: i64,
    pub(crate) provider_envelope: CredentialEnvelope,
    pub(crate) holder: String,
    pub(crate) fencing_token: i64,
    pub(crate) attempts: i32,
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
        currencies: &ProviderCurrencyMap,
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
    async fn classification_evidence(
        &self,
        user_id: UserId,
        id: ProviderEventId,
    ) -> Result<ProviderClassificationEvidence, BankingError>;
    async fn classification_evidence_for_journal(
        &self,
        user_id: UserId,
        journal_entry_id: JournalEntryId,
    ) -> Result<Option<ProviderClassificationEvidence>, BankingError>;
    async fn intake_provider_event(
        &self,
        command: IntakeProviderEvent,
    ) -> Result<ProviderEventReceipt, BankingError>;
    async fn claim_provider_import(
        &self,
        user_id: UserId,
        id: ProviderEventId,
        holder: &str,
        now: DateTime<Utc>,
        lease_seconds: i64,
    ) -> Result<Option<ProviderImportWork>, BankingError>;
    async fn next_provider_import_candidate(
        &self,
    ) -> Result<Option<(UserId, ProviderEventId)>, BankingError>;
    async fn complete_provider_import(
        &self,
        outcome: ProviderImportOutcome,
    ) -> Result<ProviderImportOutcome, BankingError>;
    async fn list_provider_event_conflicts(
        &self,
        user_id: UserId,
        connection_id: ProviderConnectionId,
    ) -> Result<Vec<ProviderEventConflictView>, BankingError>;
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
    async fn list_sync_pages(
        &self,
        user_id: UserId,
        id: SyncJobId,
    ) -> Result<Vec<SyncPageView>, BankingError>;
}

#[async_trait]
pub(crate) trait BankingWorkerRepository: Send + Sync {
    async fn claim_validation(
        &self,
        holder: &str,
        now: DateTime<Utc>,
        lease_seconds: i64,
    ) -> Result<Option<ValidationWork>, BankingError>;
    async fn complete_validation_success(
        &self,
        work: &ValidationWork,
        active_envelope: Option<&CredentialEnvelope>,
        webhook: Option<&WebhookProvisioning>,
        resources: &[NormalizedResource],
        now: DateTime<Utc>,
    ) -> Result<bool, BankingError>;
    async fn complete_validation_failure(
        &self,
        work: &ValidationWork,
        class: ProviderFailureClass,
        next_retry_at: Option<DateTime<Utc>>,
        now: DateTime<Utc>,
    ) -> Result<bool, BankingError>;
    async fn claim_webhook_registration(
        &self,
        holder: &str,
        now: DateTime<Utc>,
        lease_seconds: i64,
    ) -> Result<Option<WebhookRegistrationWork>, BankingError>;
    async fn complete_claimed_webhook_registration(
        &self,
        work: &WebhookRegistrationWork,
        failure: Option<ProviderFailureClass>,
        next_retry_at: Option<DateTime<Utc>>,
        now: DateTime<Utc>,
    ) -> Result<bool, BankingError>;
    async fn claim_webhook_receipt(
        &self,
        holder: &str,
        now: DateTime<Utc>,
        lease_seconds: i64,
    ) -> Result<Option<WebhookReceiptWork>, BankingError>;
    async fn webhook_resource_currency(
        &self,
        work: &WebhookReceiptWork,
        external_resource_id: &str,
    ) -> Result<(CurrencyCode, bool), BankingError>;
    async fn complete_webhook_receipt(
        &self,
        work: &WebhookReceiptWork,
        event: Result<(String, NormalizedProviderEvent), &'static str>,
        now: DateTime<Utc>,
    ) -> Result<bool, BankingError>;
    async fn claim_statement_window(
        &self,
        holder: &str,
        now: DateTime<Utc>,
        lease_seconds: i64,
    ) -> Result<Option<StatementWork>, BankingError>;
    async fn complete_statement_window(
        &self,
        work: &StatementWork,
        events: &[NormalizedProviderEvent],
        now: DateTime<Utc>,
    ) -> Result<u32, BankingError>;
    async fn fail_statement_window(
        &self,
        work: &StatementWork,
        class: ProviderFailureClass,
        next_retry_at: Option<DateTime<Utc>>,
        now: DateTime<Utc>,
    ) -> Result<bool, BankingError>;
    async fn finalize_one_sync_page(&self, now: DateTime<Utc>) -> Result<bool, BankingError>;
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
        holder: &str,
        now: DateTime<Utc>,
        lease_seconds: i64,
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
        cipher: &dyn CredentialCipher,
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
            || !(matches!(slot.as_str(), "active" | "pending" | "webhook")
                || slot
                    .strip_prefix("provenance:")
                    .is_some_and(|scope| !scope.is_empty() && scope.len() <= 64))
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
    fn encrypt_payload(
        &self,
        payload: &[u8],
        binding: &CredentialBinding,
    ) -> Result<CredentialEnvelope, BankingError>;
    fn decrypt_payload(
        &self,
        envelope: &CredentialEnvelope,
        binding: &CredentialBinding,
    ) -> Result<Vec<u8>, BankingError>;
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
    #[error("provider request failed ({class:?}); sensitive response omitted")]
    ClassifiedWithRetry {
        class: ProviderFailureClass,
        retry_after_seconds: u64,
    },
    #[error("provider response could not be normalized")]
    InvalidResponse,
}

impl ProviderFailure {
    pub fn class(&self) -> ProviderFailureClass {
        match self {
            Self::Classified { class } | Self::ClassifiedWithRetry { class, .. } => *class,
            Self::InvalidResponse => ProviderFailureClass::Terminal,
        }
    }

    pub fn retry_after_seconds(&self) -> Option<u64> {
        match self {
            Self::ClassifiedWithRetry {
                retry_after_seconds,
                ..
            } => Some(*retry_after_seconds),
            _ => None,
        }
    }
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

    async fn statement(
        &self,
        _credential: &ProviderCredential,
        _account: &str,
        _from: DateTime<Utc>,
        _to: DateTime<Utc>,
    ) -> Result<String, ProviderFailure> {
        Err(ProviderFailure::Classified {
            class: ProviderFailureClass::Terminal,
        })
    }
}

pub(crate) trait ProviderNormalizer: Send + Sync {
    fn client_info(
        &self,
        body: &str,
        currencies: &ProviderCurrencyMap,
    ) -> Result<NormalizedSnapshot, BankingError>;
    fn statement(
        &self,
        body: &str,
        resource_currency: &CurrencyCode,
        currencies: &ProviderCurrencyMap,
    ) -> Result<Vec<NormalizedProviderEvent>, BankingError>;
    fn webhook(
        &self,
        body: &[u8],
        resource_currency: &CurrencyCode,
        currencies: &ProviderCurrencyMap,
    ) -> Result<(String, NormalizedProviderEvent), BankingError>;
}
