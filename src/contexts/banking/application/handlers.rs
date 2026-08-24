//! Banking command/query facade with private adapters.

use std::sync::Arc;

use super::ports::{
    ConnectionRepository, CredentialBinding, CredentialCipher, NormalizedResource,
    ObservationRepository, ProviderClient, ProviderEventRepository, ResourceRepository,
    SyncJobRepository, WebhookCredential, WebhookRepository, WebhookSecrets,
};
use super::{
    AccountingProcessView, BalanceObservationDeliveryOutcome, BalanceObservationDeliveryWork,
    BalanceObservationView, BindExistingResource, ConnectProvider, ConnectionResult,
    CreateAndMapResource, DeactivateResourceMapping, ExternalResourceView, IntakeProviderEvent,
    ProviderAccountSummary, ProviderConnectionView, ProviderEventReceipt, ProviderEventView,
    ProviderImportOutcome, ProviderImportWork, RecordBalanceObservation, ReplaceProviderCredential,
    ResourceMappingResult, RotateWebhookCredential, WebhookReceiptOutcome, WebhookRotationResult,
};
use crate::contexts::banking::domain::BankingError;
use crate::shared_kernel::UserId;

#[derive(Clone)]
pub struct BankingFacade {
    pub(crate) connections: Arc<dyn ConnectionRepository>,
    pub(crate) resources: Arc<dyn ResourceRepository>,
    pub(crate) provider_events: Arc<dyn ProviderEventRepository>,
    pub(crate) sync_jobs: Arc<dyn SyncJobRepository>,
    pub(crate) observations: Arc<dyn ObservationRepository>,
    pub(crate) webhooks: Arc<dyn WebhookRepository>,
    pub(crate) cipher: Arc<dyn CredentialCipher>,
    pub(crate) provider: Arc<dyn ProviderClient>,
    pub(crate) ledger: Option<crate::contexts::ledger::public::LedgerFacade>,
    pub(crate) currencies: crate::contexts::reference_data::public::CurrencyCatalogFacade,
    pub(crate) webhook_secrets: Arc<dyn WebhookSecrets>,
}

impl BankingFacade {
    pub(crate) fn new<R>(
        repositories: Arc<R>,
        cipher: Arc<dyn CredentialCipher>,
        provider: Arc<dyn ProviderClient>,
        ledger: Option<crate::contexts::ledger::public::LedgerFacade>,
        currencies: crate::contexts::reference_data::public::CurrencyCatalogFacade,
        webhook_secrets: Arc<dyn WebhookSecrets>,
    ) -> Self
    where
        R: ConnectionRepository
            + ResourceRepository
            + ProviderEventRepository
            + SyncJobRepository
            + ObservationRepository
            + WebhookRepository
            + 'static,
    {
        Self {
            connections: repositories.clone(),
            resources: repositories.clone(),
            provider_events: repositories.clone(),
            sync_jobs: repositories.clone(),
            observations: repositories.clone(),
            webhooks: repositories,
            cipher,
            provider,
            ledger,
            currencies,
            webhook_secrets,
        }
    }

    pub async fn connect_provider(
        &self,
        command: ConnectProvider,
    ) -> Result<ConnectionResult, BankingError> {
        self.connections
            .connect(command, self.cipher.as_ref())
            .await
    }

    pub async fn replace_provider_credential(
        &self,
        command: ReplaceProviderCredential,
    ) -> Result<ConnectionResult, BankingError> {
        self.connections
            .replace_credential(command, self.cipher.as_ref())
            .await
    }

    pub async fn validate_and_discover(
        &self,
        user_id: UserId,
        connection_id: super::super::domain::ProviderConnectionId,
    ) -> Result<Vec<NormalizedResource>, BankingError> {
        use crate::contexts::reference_data::public::CurrencyCatalog;
        let currencies = self
            .currencies
            .list_enabled()
            .await
            .map_err(|_| BankingError::InvalidValue("currency catalog unavailable"))?
            .into_iter()
            .filter_map(|definition| {
                definition
                    .numeric_code
                    .and_then(|numeric| numeric.parse::<u16>().ok())
                    .map(|numeric| (numeric, (definition.code, definition.minor_unit)))
            })
            .collect();
        let resources = self
            .resources
            .validate_and_discover(
                user_id,
                connection_id,
                self.cipher.as_ref(),
                self.provider.as_ref(),
                &currencies,
            )
            .await?;
        for resource in &resources {
            let resource_id = self
                .resources
                .resource_id_by_external(user_id, connection_id, &resource.external_resource_id)
                .await?;
            let comparability =
                if resource.funding_model == super::super::domain::FundingModel::OwnFunds {
                    super::super::domain::BalanceComparability::Comparable(
                        resource.provider_balance.clone(),
                    )
                } else {
                    super::super::domain::BalanceComparability::NotComparable(
                        "provider credit balance semantics require review".to_owned(),
                    )
                };
            let now = chrono::Utc::now();
            self.observations
                .record_balance_observation(RecordBalanceObservation {
                    user_id,
                    connection_id,
                    resource_id,
                    basis: super::super::domain::BalanceBasis::Reported,
                    provider_money: resource.provider_balance.clone(),
                    sign_semantics: "provider_native".to_owned(),
                    comparability,
                    observed_at: now,
                    recorded_at: now,
                    correlation_id: crate::shared_kernel::CorrelationId::generate(),
                })
                .await?;
        }
        Ok(resources)
    }

    pub async fn list_connections(
        &self,
        user_id: UserId,
    ) -> Result<Vec<ProviderConnectionView>, BankingError> {
        self.connections.list_connections(user_id).await
    }
    pub async fn get_connection(
        &self,
        user_id: UserId,
        id: super::super::domain::ProviderConnectionId,
    ) -> Result<ProviderConnectionView, BankingError> {
        self.connections.get_connection(user_id, id).await
    }
    pub async fn disconnect(
        &self,
        user_id: UserId,
        id: super::super::domain::ProviderConnectionId,
        expected: super::super::domain::ConnectionVersion,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<ProviderConnectionView, BankingError> {
        self.connections
            .disconnect(user_id, id, expected, now)
            .await
    }
    pub async fn list_resources(
        &self,
        user_id: UserId,
        connection_id: super::super::domain::ProviderConnectionId,
    ) -> Result<Vec<ExternalResourceView>, BankingError> {
        self.resources.list_resources(user_id, connection_id).await
    }
    pub async fn require_resource_connection(
        &self,
        user_id: UserId,
        connection_id: super::super::domain::ProviderConnectionId,
        resource_id: super::super::domain::ExternalResourceId,
    ) -> Result<(), BankingError> {
        self.resources
            .require_resource_connection(user_id, connection_id, resource_id)
            .await
    }
    pub async fn resource_for_mapping(
        &self,
        user_id: UserId,
        connection_id: super::super::domain::ProviderConnectionId,
        mapping_id: super::super::domain::ResourceMappingId,
    ) -> Result<super::super::domain::ExternalResourceId, BankingError> {
        self.resources
            .resource_for_mapping(user_id, connection_id, mapping_id)
            .await
    }
    pub async fn get_provider_event(
        &self,
        user_id: UserId,
        id: super::super::domain::ProviderEventId,
    ) -> Result<ProviderEventView, BankingError> {
        self.provider_events.get_provider_event(user_id, id).await
    }
    pub async fn get_accounting_process(
        &self,
        user_id: UserId,
        id: uuid::Uuid,
    ) -> Result<AccountingProcessView, BankingError> {
        self.provider_events
            .get_accounting_process(user_id, id)
            .await
    }
    pub async fn get_balance_observation(
        &self,
        user_id: UserId,
        id: super::super::domain::BalanceObservationId,
    ) -> Result<BalanceObservationView, BankingError> {
        self.observations.get_balance_observation(user_id, id).await
    }
    pub async fn provider_account_summary(
        &self,
        user_id: UserId,
        account_id: crate::contexts::ledger::public::LedgerAccountId,
    ) -> Result<ProviderAccountSummary, BankingError> {
        self.resources
            .provider_account_summary(user_id, account_id)
            .await
    }

    pub async fn bind_existing_resource(
        &self,
        command: BindExistingResource,
    ) -> Result<ResourceMappingResult, BankingError> {
        let ledger = self.ledger.as_ref().ok_or(BankingError::InvalidState)?;
        let resource = self
            .resources
            .resource_binding(command.user_id, command.resource_id)
            .await?;
        if resource.version != command.expected_resource_version {
            return Err(BankingError::VersionConflict);
        }
        let (kind, nature) = resource.expected_ledger_account()?;
        let outcome = ledger
            .validate_provider_account_binding(
                crate::contexts::ledger::public::ValidateProviderAccountBinding {
                    user_id: command.user_id,
                    account_id: command.ledger_account_id,
                    currency: resource.currency.clone(),
                    kind,
                    nature,
                },
            )
            .await
            .map_err(|_| BankingError::IncompatibleMapping)?;
        if !matches!(
            outcome,
            crate::contexts::ledger::public::ProviderAccountBindingResult::Accepted(_)
        ) {
            return Err(BankingError::IncompatibleMapping);
        }
        self.resources.commit_mapping(command).await
    }

    pub async fn create_and_map_resource(
        &self,
        command: CreateAndMapResource,
    ) -> Result<ResourceMappingResult, BankingError> {
        let ledger = self.ledger.as_ref().ok_or(BankingError::InvalidState)?;
        let pending = self.resources.ensure_pending_mapping(&command).await?;
        if pending.mapping.ledger_account_id.is_some() && pending.mapping.state == "active" {
            return Ok(ResourceMappingResult {
                replayed: true,
                ..pending
            });
        }
        let resource = self
            .resources
            .resource_binding(command.user_id, command.resource_id)
            .await?;
        let (kind, nature) = resource.expected_ledger_account()?;
        let opened = ledger
            .open_provider_observed_account(
                crate::contexts::ledger::public::OpenProviderObservedAccount {
                    user_id: command.user_id,
                    name: command.account_name.clone(),
                    currency: resource.currency,
                    kind,
                    nature,
                    source: crate::contexts::ledger::public::SourceReference::new(
                        "banking",
                        command.resource_id.to_string(),
                        format!("mapping:{}", pending.mapping.mapping_version),
                    )
                    .map_err(|_| BankingError::InvalidValue("invalid mapping source"))?,
                    idempotency_key: crate::shared_kernel::IdempotencyKey::new(format!(
                        "banking-resource-account:{}:{}",
                        command.resource_id, pending.mapping.mapping_version
                    ))
                    .map_err(|_| BankingError::InvalidValue("invalid mapping idempotency key"))?,
                    correlation_id: command.correlation_id,
                    causation_id: None,
                    occurred_at: command.requested_at,
                },
            )
            .await
            .map_err(|_| BankingError::IncompatibleMapping)?;
        self.resources
            .complete_pending_mapping(
                command.user_id,
                command.resource_id,
                pending.mapping.id,
                pending.mapping.mapping_version,
                opened.account.id,
                command.requested_at,
            )
            .await
    }

    pub async fn deactivate_resource_mapping(
        &self,
        command: DeactivateResourceMapping,
    ) -> Result<ResourceMappingResult, BankingError> {
        self.resources.deactivate_mapping(command).await
    }

    pub async fn intake_provider_event(
        &self,
        command: IntakeProviderEvent,
    ) -> Result<ProviderEventReceipt, BankingError> {
        self.provider_events.intake_provider_event(command).await
    }

    pub async fn claim_provider_import(
        &self,
        user_id: UserId,
        provider_event_id: super::super::domain::ProviderEventId,
    ) -> Result<Option<ProviderImportWork>, BankingError> {
        self.provider_events
            .claim_provider_import(user_id, provider_event_id)
            .await
    }

    pub async fn next_provider_import_candidate(
        &self,
    ) -> Result<Option<(UserId, super::super::domain::ProviderEventId)>, BankingError> {
        self.provider_events.next_provider_import_candidate().await
    }

    pub async fn complete_provider_import(
        &self,
        outcome: ProviderImportOutcome,
    ) -> Result<ProviderImportOutcome, BankingError> {
        self.provider_events.complete_provider_import(outcome).await
    }

    pub async fn record_balance_observation(
        &self,
        command: RecordBalanceObservation,
    ) -> Result<BalanceObservationView, BankingError> {
        self.observations.record_balance_observation(command).await
    }

    pub async fn claim_balance_observation(
        &self,
        user_id: UserId,
        observation_id: super::super::domain::BalanceObservationId,
    ) -> Result<Option<BalanceObservationDeliveryWork>, BankingError> {
        self.observations
            .claim_balance_observation(user_id, observation_id)
            .await
    }

    pub async fn next_balance_observation_candidate(
        &self,
    ) -> Result<Option<(UserId, super::super::domain::BalanceObservationId)>, BankingError> {
        self.observations.next_balance_observation_candidate().await
    }

    pub async fn complete_balance_observation(
        &self,
        outcome: BalanceObservationDeliveryOutcome,
    ) -> Result<BalanceObservationDeliveryOutcome, BankingError> {
        self.observations
            .complete_balance_observation(outcome)
            .await
    }

    pub async fn rotate_webhook_credential(
        &self,
        command: RotateWebhookCredential,
    ) -> Result<WebhookRotationResult, BankingError> {
        let credential = self.webhook_secrets.generate();
        let digest = self.webhook_secrets.digest(&credential);
        self.webhooks
            .rotate_webhook(command, credential, digest, self.cipher.as_ref())
            .await
    }

    pub async fn validate_webhook_credential(
        &self,
        credential: &str,
    ) -> Result<bool, BankingError> {
        let credential = WebhookCredential::new(credential)?;
        self.webhooks
            .validate_webhook_digest(
                &self.webhook_secrets.digest(&credential),
                self.webhook_secrets.as_ref(),
            )
            .await
    }

    pub async fn receive_webhook(
        &self,
        credential: &str,
        body: &[u8],
    ) -> Result<WebhookReceiptOutcome, BankingError> {
        if body.len() > 1_048_576 {
            return Err(BankingError::InvalidValue("webhook body is too large"));
        }
        let credential = WebhookCredential::new(credential)?;
        self.webhooks
            .receive_webhook(
                &self.webhook_secrets.digest(&credential),
                body,
                self.webhook_secrets.as_ref(),
            )
            .await
    }

    pub async fn register_pending_webhook(
        &self,
        user_id: UserId,
        connection_id: super::super::domain::ProviderConnectionId,
        callback_base: &str,
    ) -> Result<(), BankingError> {
        let work = self
            .webhooks
            .webhook_registration_work(user_id, connection_id)
            .await?;
        let token = self.cipher.decrypt(
            &work.provider_envelope,
            &CredentialBinding::new(
                user_id,
                connection_id.into_uuid(),
                &work.provider,
                work.credential_generation,
                "active",
            )?,
        )?;
        let webhook = self.cipher.decrypt(
            &work.webhook_envelope,
            &CredentialBinding::new(
                user_id,
                connection_id.into_uuid(),
                &work.provider,
                work.webhook_version,
                "webhook",
            )?,
        )?;
        let url = format!(
            "{}/webhooks/monobank/{}",
            callback_base.trim_end_matches('/'),
            webhook.expose()
        );
        match self.provider.register_webhook(&token, &url).await {
            Ok(()) => {
                self.webhooks
                    .complete_webhook_registration(
                        user_id,
                        connection_id,
                        work.webhook_version,
                        true,
                    )
                    .await
            }
            Err(_) => {
                self.webhooks
                    .complete_webhook_registration(
                        user_id,
                        connection_id,
                        work.webhook_version,
                        false,
                    )
                    .await
            }
        }
    }
}
