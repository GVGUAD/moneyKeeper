//! PostgreSQL Banking store.

use std::collections::BTreeMap;

use serde_json::json;
use sha2::{Digest, Sha256};
use sqlx::Row;

use crate::{contexts::banking::{application::{BindExistingResource, ConnectProvider, ConnectionResult, CreateAndMapResource, CredentialBinding, CredentialCipher, DeactivateResourceMapping, IntakeProviderEvent, ProviderClient, ProviderConnectionView, ProviderEventIntakeOutcome, ProviderEventReadyV1, ProviderEventReceipt, ReplaceProviderCredential, ResourceMappingResult, ResourceMappingView}, domain::{BankingError, ConnectionState, ConnectionVersion, CredentialEnvelope, ExternalResourceId, FundingModel, ProviderConnectionId, ProviderEventId, ProviderTransactionState, ResourceKind, ResourceMappingId}}, contexts::ledger::public::{AccountKind, AccountNature, LedgerAccountId}, infrastructure::v2_db::VerifiedV2Pool, integration::{IntegrationEvent, outbox::OutboxWriter, postgres::PgOutboxWriter}, shared_kernel::{CurrencyCode, EventEnvelope, EventId, UserId}};

use super::{MonobankAdapter, NormalizedResource, pg_unit_of_work::PgBankingUnitOfWork, rows::ConnectionRow};

#[derive(Clone)]
pub(crate) struct PgBankingStore { uow: PgBankingUnitOfWork }

impl PgBankingStore {
    pub(crate) fn new(pool: &VerifiedV2Pool) -> Self { Self { uow: PgBankingUnitOfWork { pool: pool.pool().clone() } } }

    pub(crate) async fn connect(&self, command: ConnectProvider, cipher: &dyn CredentialCipher) -> Result<ConnectionResult, BankingError> {
        if command.provider != "monobank" { return Err(BankingError::InvalidValue("unsupported provider")); }
        let request_hash = Sha256::digest(json!({"provider":command.provider,"requested_at":command.requested_at}).to_string().as_bytes()).to_vec();
        let mut tx = self.uow.pool.begin().await.map_err(database)?;
        if let Some(row) = sqlx::query("SELECT request_hash,result FROM banking.command_receipts WHERE user_id=$1 AND scope='connect_provider' AND idempotency_key=$2")
            .bind(command.user_id.into_uuid()).bind(command.idempotency_key.as_str()).fetch_optional(&mut *tx).await.map_err(database)? {
            if row.get::<Vec<u8>, _>("request_hash") != request_hash { return Err(BankingError::InvalidValue("idempotency conflict")); }
            let result: ConnectionResult = serde_json::from_value(row.get("result")).map_err(|_| BankingError::InvalidValue("stored command result is invalid"))?;
            tx.rollback().await.map_err(database)?;
            return Ok(ConnectionResult { replayed: true, ..result });
        }
        let connection_id = ProviderConnectionId::generate();
        let binding = CredentialBinding::new(command.user_id, connection_id.into_uuid(), &command.provider, 1, "active")?;
        let envelope = cipher.encrypt(&command.credential, &binding)?;
        sqlx::query("INSERT INTO banking.provider_connections (id,user_id,provider,state,active_credential_ciphertext,active_credential_nonce,active_credential_key_id,active_credential_envelope_version) VALUES ($1,$2,$3,'pending',$4,$5,$6,$7)")
            .bind(connection_id.into_uuid()).bind(command.user_id.into_uuid()).bind(&command.provider).bind(envelope.ciphertext()).bind(envelope.nonce()).bind(envelope.key_id()).bind(i16::try_from(envelope.envelope_version()).unwrap()).execute(&mut *tx).await.map_err(database)?;
        let view = ProviderConnectionView { id: connection_id, user_id: command.user_id, provider: command.provider, state: ConnectionState::Pending, credential_generation: 1, version: ConnectionVersion::INITIAL, webhook_configured: false, created_at: command.requested_at, updated_at: command.requested_at };
        let result = ConnectionResult { connection: view, replayed: false };
        sqlx::query("INSERT INTO banking.command_receipts (user_id,scope,idempotency_key,request_hash,result,status_code) VALUES ($1,'connect_provider',$2,$3,$4,202)")
            .bind(command.user_id.into_uuid()).bind(command.idempotency_key.as_str()).bind(request_hash).bind(serde_json::to_value(&result).map_err(|_| BankingError::InvalidValue("cannot serialize command result"))?).execute(&mut *tx).await.map_err(database)?;
        tx.commit().await.map_err(database)?;
        Ok(result)
    }

    pub(crate) async fn replace_credential(&self, command: ReplaceProviderCredential, cipher: &dyn CredentialCipher) -> Result<ConnectionResult, BankingError> {
        let mut tx = self.uow.pool.begin().await.map_err(database)?;
        let row = sqlx::query("SELECT provider,state,credential_generation,version,created_at FROM banking.provider_connections WHERE id=$1 AND user_id=$2 FOR UPDATE")
            .bind(command.connection_id.into_uuid()).bind(command.user_id.into_uuid()).fetch_optional(&mut *tx).await.map_err(database)?.ok_or(BankingError::InvalidState)?;
        let version: i64 = row.get("version");
        if version != command.expected_version.get() { return Err(BankingError::VersionConflict); }
        let generation: i64 = row.get::<i64,_>("credential_generation").checked_add(1).ok_or(BankingError::InvalidValue("credential generation overflow"))?;
        let provider: String = row.get("provider");
        let binding = CredentialBinding::new(command.user_id, command.connection_id.into_uuid(), &provider, generation, "pending")?;
        let envelope = cipher.encrypt(&command.credential, &binding)?;
        sqlx::query("UPDATE banking.provider_connections SET pending_credential_ciphertext=$3,pending_credential_nonce=$4,pending_credential_key_id=$5,pending_credential_envelope_version=$6,state='pending_credential_validation',version=version+1,updated_at=$7 WHERE id=$1 AND user_id=$2 AND version=$8")
            .bind(command.connection_id.into_uuid()).bind(command.user_id.into_uuid()).bind(envelope.ciphertext()).bind(envelope.nonce()).bind(envelope.key_id()).bind(i16::try_from(envelope.envelope_version()).unwrap()).bind(command.requested_at).bind(version).execute(&mut *tx).await.map_err(database)?;
        tx.commit().await.map_err(database)?;
        Ok(ConnectionResult { connection: ProviderConnectionView { id: command.connection_id, user_id: command.user_id, provider, state: ConnectionState::PendingCredentialValidation, credential_generation: generation, version: ConnectionVersion::new(version+1)?, webhook_configured: false, created_at: row.get("created_at"), updated_at: command.requested_at }, replayed: false })
    }

    pub(crate) async fn validate_and_discover(&self, user_id: UserId, connection_id: ProviderConnectionId, cipher: &dyn CredentialCipher, provider_client: &dyn ProviderClient, currencies: &BTreeMap<u16, (CurrencyCode, u8)>) -> Result<Vec<NormalizedResource>, BankingError> {
        let row = self.connection_row(user_id, connection_id).await?;
        let candidate = row.state == "pending_credential_validation";
        let generation = if candidate { row.credential_generation.checked_add(1).ok_or(BankingError::InvalidValue("credential generation overflow"))? } else { row.credential_generation };
        let (key_id, nonce, ciphertext, envelope_version, slot) = if candidate {
            (row.pending_credential_key_id, row.pending_credential_nonce, row.pending_credential_ciphertext, row.pending_credential_envelope_version, "pending")
        } else {
            (row.active_credential_key_id, row.active_credential_nonce, row.active_credential_ciphertext, row.active_credential_envelope_version, "active")
        };
        let envelope = CredentialEnvelope::new(key_id.ok_or(BankingError::CredentialUnavailable)?, nonce.ok_or(BankingError::CredentialUnavailable)?, ciphertext.ok_or(BankingError::CredentialUnavailable)?)?;
        if envelope_version != Some(1) { return Err(BankingError::CredentialUnavailable); }
        let binding = CredentialBinding::new(user_id, connection_id.into_uuid(), &row.provider, generation, slot)?;
        let credential = cipher.decrypt(&envelope, &binding)?;
        let body = match provider_client.client_info(&credential).await {
            Ok(body) => body,
            Err(_) if candidate => {
                sqlx::query("UPDATE banking.provider_connections SET pending_credential_ciphertext=NULL,pending_credential_nonce=NULL,pending_credential_key_id=NULL,pending_credential_envelope_version=NULL,state=CASE WHEN active_credential_ciphertext IS NULL THEN 'needs_reauth' ELSE 'active' END,version=version+1,updated_at=clock_timestamp() WHERE id=$1 AND user_id=$2 AND state='pending_credential_validation'")
                    .bind(connection_id.into_uuid()).bind(user_id.into_uuid()).execute(&self.uow.pool).await.map_err(database)?;
                return Err(BankingError::InvalidValue("provider validation failed"));
            }
            Err(_) => return Err(BankingError::InvalidValue("provider validation failed")),
        };
        let snapshot = MonobankAdapter::normalize_client_info(&body, currencies)?;
        let mut tx = self.uow.pool.begin().await.map_err(database)?;
        for resource in &snapshot.resources {
            sqlx::query("INSERT INTO banking.external_resources (id,user_id,connection_id,external_resource_id,kind,funding_model,currency,masked_label,discovery_state) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9) ON CONFLICT (connection_id,external_resource_id) DO UPDATE SET masked_label=EXCLUDED.masked_label,funding_model=EXCLUDED.funding_model,discovery_state=EXCLUDED.discovery_state,version=banking.external_resources.version+1,updated_at=clock_timestamp()")
                .bind(uuid::Uuid::new_v4()).bind(user_id.into_uuid()).bind(connection_id.into_uuid()).bind(&resource.external_resource_id).bind(kind(resource.kind)).bind(funding(resource.funding_model)).bind(resource.currency.as_str()).bind(&resource.masked_label).bind(if resource.kind == crate::contexts::banking::domain::ResourceKind::Unsupported {"unsupported"} else {"active"}).execute(&mut *tx).await.map_err(database)?;
        }
        if candidate {
            sqlx::query("UPDATE banking.provider_connections SET active_credential_ciphertext=pending_credential_ciphertext,active_credential_nonce=pending_credential_nonce,active_credential_key_id=pending_credential_key_id,active_credential_envelope_version=pending_credential_envelope_version,pending_credential_ciphertext=NULL,pending_credential_nonce=NULL,pending_credential_key_id=NULL,pending_credential_envelope_version=NULL,credential_generation=$3,state='active',version=version+1,updated_at=clock_timestamp() WHERE id=$1 AND user_id=$2 AND credential_generation=$4 AND state='pending_credential_validation'")
                .bind(connection_id.into_uuid()).bind(user_id.into_uuid()).bind(generation).bind(row.credential_generation).execute(&mut *tx).await.map_err(database)?;
        } else {
            sqlx::query("UPDATE banking.provider_connections SET state='active',version=version+1,updated_at=clock_timestamp() WHERE id=$1 AND user_id=$2 AND credential_generation=$3")
                .bind(connection_id.into_uuid()).bind(user_id.into_uuid()).bind(row.credential_generation).execute(&mut *tx).await.map_err(database)?;
        }
        tx.commit().await.map_err(database)?;
        Ok(snapshot.resources)
    }

    pub(crate) async fn list_connections(&self, user_id: UserId) -> Result<Vec<ProviderConnectionView>, BankingError> {
        let rows = sqlx::query("SELECT id,user_id,provider,state,credential_generation,version,webhook_lookup_digest,created_at,updated_at FROM banking.provider_connections WHERE user_id=$1 ORDER BY created_at,id").bind(user_id.into_uuid()).fetch_all(&self.uow.pool).await.map_err(database)?;
        rows.into_iter().map(view).collect()
    }

    pub(crate) async fn resource_binding(&self, user_id: UserId, resource_id: ExternalResourceId) -> Result<ResourceBinding, BankingError> {
        let row = sqlx::query("SELECT kind,funding_model,currency,version FROM banking.external_resources WHERE id=$1 AND user_id=$2 AND discovery_state IN ('active','needs_review')")
            .bind(resource_id.into_uuid()).bind(user_id.into_uuid()).fetch_optional(&self.uow.pool).await.map_err(database)?.ok_or(BankingError::InvalidState)?;
        Ok(ResourceBinding { kind: parse_kind(row.get::<String,_>("kind").as_str())?, funding_model: parse_funding(row.get::<String,_>("funding_model").as_str())?, currency: CurrencyCode::new(row.get::<String,_>("currency")).map_err(|_| BankingError::InvalidValue("stored resource currency is invalid"))?, version: row.get("version") })
    }

    pub(crate) async fn commit_mapping(&self, command: BindExistingResource) -> Result<ResourceMappingResult, BankingError> {
        let mut tx = self.uow.pool.begin().await.map_err(database)?;
        let version: i64 = sqlx::query_scalar("SELECT version FROM banking.external_resources WHERE id=$1 AND user_id=$2 FOR UPDATE")
            .bind(command.resource_id.into_uuid()).bind(command.user_id.into_uuid()).fetch_optional(&mut *tx).await.map_err(database)?.ok_or(BankingError::InvalidState)?;
        if version != command.expected_resource_version { return Err(BankingError::VersionConflict); }
        if let Some(row) = sqlx::query("SELECT id,ledger_account_id,mapping_version,state,effective_at,ended_at FROM banking.resource_mappings WHERE external_resource_id=$1 AND user_id=$2 AND state='active'")
            .bind(command.resource_id.into_uuid()).bind(command.user_id.into_uuid()).fetch_optional(&mut *tx).await.map_err(database)? {
            let mapping = mapping_view(command.resource_id, row)?;
            tx.rollback().await.map_err(database)?;
            return if mapping.ledger_account_id == Some(command.ledger_account_id) { Ok(ResourceMappingResult { mapping, replayed: true }) } else { Err(BankingError::MappingAlreadyActive) };
        }
        let mapping_version: i64 = sqlx::query_scalar("SELECT COALESCE(max(mapping_version),0)+1 FROM banking.resource_mappings WHERE external_resource_id=$1 AND user_id=$2")
            .bind(command.resource_id.into_uuid()).bind(command.user_id.into_uuid()).fetch_one(&mut *tx).await.map_err(database)?;
        let id = ResourceMappingId::generate();
        sqlx::query("INSERT INTO banking.resource_mappings (id,user_id,connection_id,external_resource_id,ledger_account_id,mapping_version,state,effective_at) SELECT $1,$2,connection_id,id,$3,$4,'active',$5 FROM banking.external_resources WHERE id=$6 AND user_id=$2")
            .bind(id.into_uuid()).bind(command.user_id.into_uuid()).bind(command.ledger_account_id.into_uuid()).bind(mapping_version).bind(command.requested_at).bind(command.resource_id.into_uuid()).execute(&mut *tx).await.map_err(database)?;
        sqlx::query("UPDATE banking.external_resources SET version=version+1,updated_at=$3 WHERE id=$1 AND user_id=$2 AND version=$4")
            .bind(command.resource_id.into_uuid()).bind(command.user_id.into_uuid()).bind(command.requested_at).bind(version).execute(&mut *tx).await.map_err(database)?;
        tx.commit().await.map_err(database)?;
        Ok(ResourceMappingResult { mapping: ResourceMappingView { id, resource_id: command.resource_id, ledger_account_id: Some(command.ledger_account_id), mapping_version, state: "active".to_owned(), effective_at: command.requested_at, ended_at: None }, replayed: false })
    }

    pub(crate) async fn ensure_pending_mapping(&self, command: &CreateAndMapResource) -> Result<ResourceMappingResult, BankingError> {
        let mut tx = self.uow.pool.begin().await.map_err(database)?;
        let version: i64 = sqlx::query_scalar("SELECT version FROM banking.external_resources WHERE id=$1 AND user_id=$2 FOR UPDATE")
            .bind(command.resource_id.into_uuid()).bind(command.user_id.into_uuid()).fetch_optional(&mut *tx).await.map_err(database)?.ok_or(BankingError::InvalidState)?;
        if version != command.expected_resource_version && !sqlx::query_scalar::<_,bool>("SELECT EXISTS(SELECT 1 FROM banking.resource_mappings WHERE external_resource_id=$1 AND user_id=$2 AND state IN ('pending_account_creation','active'))").bind(command.resource_id.into_uuid()).bind(command.user_id.into_uuid()).fetch_one(&mut *tx).await.map_err(database)? { return Err(BankingError::VersionConflict); }
        if let Some(row) = sqlx::query("SELECT id,ledger_account_id,mapping_version,state,effective_at,ended_at FROM banking.resource_mappings WHERE external_resource_id=$1 AND user_id=$2 AND state IN ('pending_account_creation','active') ORDER BY mapping_version DESC LIMIT 1")
            .bind(command.resource_id.into_uuid()).bind(command.user_id.into_uuid()).fetch_optional(&mut *tx).await.map_err(database)? {
            let mapping = mapping_view(command.resource_id, row)?; tx.rollback().await.map_err(database)?; return Ok(ResourceMappingResult { mapping, replayed: true });
        }
        let mapping_version: i64 = sqlx::query_scalar("SELECT COALESCE(max(mapping_version),0)+1 FROM banking.resource_mappings WHERE external_resource_id=$1 AND user_id=$2").bind(command.resource_id.into_uuid()).bind(command.user_id.into_uuid()).fetch_one(&mut *tx).await.map_err(database)?;
        let id=ResourceMappingId::generate();
        sqlx::query("INSERT INTO banking.resource_mappings (id,user_id,connection_id,external_resource_id,ledger_account_id,mapping_version,state,process_correlation_id,effective_at) SELECT $1,$2,connection_id,id,NULL,$3,'pending_account_creation',$4,$5 FROM banking.external_resources WHERE id=$6 AND user_id=$2")
            .bind(id.into_uuid()).bind(command.user_id.into_uuid()).bind(mapping_version).bind(command.correlation_id.into_uuid()).bind(command.requested_at).bind(command.resource_id.into_uuid()).execute(&mut *tx).await.map_err(database)?;
        sqlx::query("UPDATE banking.external_resources SET version=version+1,updated_at=$3 WHERE id=$1 AND user_id=$2 AND version=$4").bind(command.resource_id.into_uuid()).bind(command.user_id.into_uuid()).bind(command.requested_at).bind(version).execute(&mut *tx).await.map_err(database)?;
        tx.commit().await.map_err(database)?;
        Ok(ResourceMappingResult { mapping: ResourceMappingView { id, resource_id: command.resource_id, ledger_account_id: None, mapping_version, state:"pending_account_creation".to_owned(), effective_at:command.requested_at, ended_at:None }, replayed:false })
    }

    pub(crate) async fn complete_pending_mapping(&self, user_id: UserId, resource_id: ExternalResourceId, mapping_id: ResourceMappingId, mapping_version: i64, account_id: LedgerAccountId, now: chrono::DateTime<chrono::Utc>) -> Result<ResourceMappingResult, BankingError> {
        let result=sqlx::query("UPDATE banking.resource_mappings SET ledger_account_id=$4,state='active',updated_at=$5 WHERE id=$1 AND user_id=$2 AND external_resource_id=$3 AND mapping_version=$6 AND state='pending_account_creation' RETURNING id,ledger_account_id,mapping_version,state,effective_at,ended_at")
            .bind(mapping_id.into_uuid()).bind(user_id.into_uuid()).bind(resource_id.into_uuid()).bind(account_id.into_uuid()).bind(now).bind(mapping_version).fetch_optional(&self.uow.pool).await.map_err(database)?;
        let replayed = result.is_none();
        let row=match result { Some(row)=>row, None=>sqlx::query("SELECT id,ledger_account_id,mapping_version,state,effective_at,ended_at FROM banking.resource_mappings WHERE id=$1 AND user_id=$2 AND state='active'").bind(mapping_id.into_uuid()).bind(user_id.into_uuid()).fetch_optional(&self.uow.pool).await.map_err(database)?.ok_or(BankingError::VersionConflict)? };
        Ok(ResourceMappingResult { mapping:mapping_view(resource_id,row)?, replayed })
    }

    pub(crate) async fn deactivate_mapping(&self, command: DeactivateResourceMapping) -> Result<ResourceMappingResult, BankingError> {
        if command.reason.trim().is_empty() || command.reason.len()>500 { return Err(BankingError::InvalidValue("mapping reason is invalid")); }
        let mut tx=self.uow.pool.begin().await.map_err(database)?;
        let version:i64=sqlx::query_scalar("SELECT version FROM banking.external_resources WHERE id=$1 AND user_id=$2 FOR UPDATE").bind(command.resource_id.into_uuid()).bind(command.user_id.into_uuid()).fetch_optional(&mut *tx).await.map_err(database)?.ok_or(BankingError::InvalidState)?;
        if version!=command.expected_resource_version { return Err(BankingError::VersionConflict); }
        let row=sqlx::query("UPDATE banking.resource_mappings SET state='inactive',reason=$3,ended_at=$4,updated_at=$4 WHERE external_resource_id=$1 AND user_id=$2 AND state IN ('active','needs_review') RETURNING id,ledger_account_id,mapping_version,state,effective_at,ended_at").bind(command.resource_id.into_uuid()).bind(command.user_id.into_uuid()).bind(&command.reason).bind(command.requested_at).fetch_optional(&mut *tx).await.map_err(database)?.ok_or(BankingError::MappingNotActive)?;
        sqlx::query("UPDATE banking.external_resources SET version=version+1,updated_at=$3 WHERE id=$1 AND user_id=$2 AND version=$4").bind(command.resource_id.into_uuid()).bind(command.user_id.into_uuid()).bind(command.requested_at).bind(version).execute(&mut *tx).await.map_err(database)?;
        tx.commit().await.map_err(database)?;
        Ok(ResourceMappingResult { mapping:mapping_view(command.resource_id,row)?, replayed:false })
    }

    pub(crate) async fn intake_provider_event(&self, command: IntakeProviderEvent) -> Result<ProviderEventReceipt, BankingError> {
        if command.external_event_id.is_empty() || command.external_event_id.len()>200 || command.revision<1 || command.operation_money.is_zero() || command.effective_at>command.recorded_at { return Err(BankingError::InvalidValue("invalid provider event intake")); }
        let content = json!({"connection_id":command.connection_id,"resource_id":command.resource_id,"external_event_id":command.external_event_id,"revision":command.revision,"state":command.state,"operation_money":command.operation_money,"description":command.description,"effective_at":command.effective_at});
        let digest=Sha256::digest(serde_json::to_vec(&content).map_err(|_| BankingError::InvalidValue("cannot canonicalize provider event"))?).to_vec();
        let mut tx=self.uow.pool.begin().await.map_err(database)?;
        if let Some(row)=sqlx::query("SELECT id,content_digest FROM banking.provider_events WHERE connection_id=$1 AND external_resource_id=$2 AND external_event_id=$3 AND revision=$4 FOR UPDATE")
            .bind(command.connection_id.into_uuid()).bind(command.resource_id.into_uuid()).bind(&command.external_event_id).bind(command.revision).fetch_optional(&mut *tx).await.map_err(database)? {
            let id=ProviderEventId::new(row.get("id"));
            if row.get::<Vec<u8>,_>("content_digest")==digest { tx.rollback().await.map_err(database)?; return Ok(ProviderEventReceipt{provider_event_id:id,outcome:ProviderEventIntakeOutcome::Duplicate,processing_state:"ready".to_owned()}); }
            sqlx::query("INSERT INTO banking.provider_event_conflicts (id,user_id,provider_event_id,conflicting_digest,reason) VALUES ($1,$2,$3,$4,'same provider revision arrived with different normalized content') ON CONFLICT (provider_event_id,conflicting_digest) DO UPDATE SET reason=banking.provider_event_conflicts.reason")
                .bind(uuid::Uuid::new_v4()).bind(command.user_id.into_uuid()).bind(id.into_uuid()).bind(&digest).execute(&mut *tx).await.map_err(database)?;
            sqlx::query("UPDATE banking.provider_event_processes SET state='quarantined',last_error='conflicting normalized content',process_version=process_version+1,updated_at=$3 WHERE provider_event_id=$1 AND user_id=$2")
                .bind(id.into_uuid()).bind(command.user_id.into_uuid()).bind(command.recorded_at).execute(&mut *tx).await.map_err(database)?;
            tx.commit().await.map_err(database)?;
            return Ok(ProviderEventReceipt{provider_event_id:id,outcome:ProviderEventIntakeOutcome::ConflictingContent,processing_state:"quarantined".to_owned()});
        }
        let id=ProviderEventId::generate();
        sqlx::query("INSERT INTO banking.provider_events (id,user_id,connection_id,external_resource_id,external_event_id,revision,transaction_state,operation_amount,operation_currency,description,content_digest,effective_at,recorded_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13)")
            .bind(id.into_uuid()).bind(command.user_id.into_uuid()).bind(command.connection_id.into_uuid()).bind(command.resource_id.into_uuid()).bind(&command.external_event_id).bind(command.revision).bind(transaction_state(command.state)).bind(command.operation_money.amount()).bind(command.operation_money.currency().as_str()).bind(&command.description).bind(&digest).bind(command.effective_at).bind(command.recorded_at).execute(&mut *tx).await.map_err(database)?;
        sqlx::query("INSERT INTO banking.provider_event_processes (provider_event_id,user_id,state) VALUES ($1,$2,'ready')").bind(id.into_uuid()).bind(command.user_id.into_uuid()).execute(&mut *tx).await.map_err(database)?;
        let payload=ProviderEventReadyV1{provider_event_id:id,connection_id:command.connection_id,resource_id:command.resource_id,external_event_id:command.external_event_id,revision:command.revision};
        let envelope=EventEnvelope::new(EventId::generate(),"banking",id.to_string(),1,"banking.provider-event-ready.v1",1,command.user_id,command.recorded_at,command.correlation_id,None).map_err(|_| BankingError::InvalidValue("cannot create provider event envelope"))?;
        PgOutboxWriter::from_transaction(&mut tx).append(&IntegrationEvent::new(envelope,serde_json::to_value(payload).map_err(|_| BankingError::InvalidValue("cannot serialize provider event"))?)).await.map_err(|_| BankingError::InvalidValue("cannot append provider event outbox"))?;
        tx.commit().await.map_err(database)?;
        Ok(ProviderEventReceipt{provider_event_id:id,outcome:ProviderEventIntakeOutcome::New,processing_state:"ready".to_owned()})
    }

    async fn connection_row(&self, user_id: UserId, connection_id: ProviderConnectionId) -> Result<ConnectionRow, BankingError> {
        let row = sqlx::query("SELECT provider,state,active_credential_ciphertext,active_credential_nonce,active_credential_key_id,active_credential_envelope_version,pending_credential_ciphertext,pending_credential_nonce,pending_credential_key_id,pending_credential_envelope_version,credential_generation FROM banking.provider_connections WHERE id=$1 AND user_id=$2")
            .bind(connection_id.into_uuid()).bind(user_id.into_uuid()).fetch_optional(&self.uow.pool).await.map_err(database)?.ok_or(BankingError::InvalidState)?;
        Ok(ConnectionRow { provider: row.get("provider"), state: row.get("state"), active_credential_ciphertext: row.get("active_credential_ciphertext"), active_credential_nonce: row.get("active_credential_nonce"), active_credential_key_id: row.get("active_credential_key_id"), active_credential_envelope_version: row.get("active_credential_envelope_version"), pending_credential_ciphertext: row.get("pending_credential_ciphertext"), pending_credential_nonce: row.get("pending_credential_nonce"), pending_credential_key_id: row.get("pending_credential_key_id"), pending_credential_envelope_version: row.get("pending_credential_envelope_version"), credential_generation: row.get("credential_generation") })
    }
}

pub(crate) struct ResourceBinding { pub(crate) kind: ResourceKind, pub(crate) funding_model: FundingModel, pub(crate) currency: CurrencyCode, pub(crate) version: i64 }
impl ResourceBinding { pub(crate) fn expected_ledger_account(&self) -> Result<(AccountKind,AccountNature),BankingError> { match (self.kind,self.funding_model) { (ResourceKind::Card,FundingModel::OwnFunds)=>Ok((AccountKind::DebitCard,AccountNature::Asset)),(ResourceKind::CurrentAccount,FundingModel::OwnFunds)=>Ok((AccountKind::Current,AccountNature::Asset)),(ResourceKind::Jar,FundingModel::OwnFunds)=>Ok((AccountKind::Jar,AccountNature::Asset)),(ResourceKind::Card,FundingModel::RevolvingCredit)=>Ok((AccountKind::CreditCard,AccountNature::Liability)),(ResourceKind::SecurityPortfolio,_)=>Err(BankingError::RouteToPortfolio),_=>Err(BankingError::IncompatibleMapping) } } }

fn view(row: sqlx::postgres::PgRow) -> Result<ProviderConnectionView, BankingError> {
    Ok(ProviderConnectionView { id: ProviderConnectionId::new(row.get("id")), user_id: UserId::new(row.get("user_id")), provider: row.get("provider"), state: state(row.get::<String,_>("state").as_str())?, credential_generation: row.get("credential_generation"), version: ConnectionVersion::new(row.get("version"))?, webhook_configured: row.get::<Option<Vec<u8>>,_>("webhook_lookup_digest").is_some(), created_at: row.get("created_at"), updated_at: row.get("updated_at") })
}
fn state(value: &str) -> Result<ConnectionState, BankingError> { match value { "pending"=>Ok(ConnectionState::Pending),"active"=>Ok(ConnectionState::Active),"pending_credential_validation"=>Ok(ConnectionState::PendingCredentialValidation),"needs_reauth"=>Ok(ConnectionState::NeedsReauth),"revoked"=>Ok(ConnectionState::Revoked),_=>Err(BankingError::InvalidValue("stored connection state is invalid")) } }
fn kind(value: crate::contexts::banking::domain::ResourceKind) -> &'static str { match value { crate::contexts::banking::domain::ResourceKind::Card=>"card",crate::contexts::banking::domain::ResourceKind::CurrentAccount=>"current_account",crate::contexts::banking::domain::ResourceKind::Jar=>"jar",crate::contexts::banking::domain::ResourceKind::SecurityPortfolio=>"security_portfolio",crate::contexts::banking::domain::ResourceKind::Unsupported=>"unsupported" } }
fn funding(value: crate::contexts::banking::domain::FundingModel) -> &'static str { match value { crate::contexts::banking::domain::FundingModel::OwnFunds=>"own_funds",crate::contexts::banking::domain::FundingModel::RevolvingCredit=>"revolving_credit",crate::contexts::banking::domain::FundingModel::Unknown=>"unknown" } }
fn parse_kind(value:&str)->Result<ResourceKind,BankingError>{match value{"card"=>Ok(ResourceKind::Card),"current_account"=>Ok(ResourceKind::CurrentAccount),"jar"=>Ok(ResourceKind::Jar),"security_portfolio"=>Ok(ResourceKind::SecurityPortfolio),"unsupported"=>Ok(ResourceKind::Unsupported),_=>Err(BankingError::InvalidValue("stored resource kind is invalid"))}}
fn parse_funding(value:&str)->Result<FundingModel,BankingError>{match value{"own_funds"=>Ok(FundingModel::OwnFunds),"revolving_credit"=>Ok(FundingModel::RevolvingCredit),"unknown"=>Ok(FundingModel::Unknown),_=>Err(BankingError::InvalidValue("stored funding model is invalid"))}}
fn transaction_state(value:ProviderTransactionState)->&'static str{match value{ProviderTransactionState::Pending=>"pending",ProviderTransactionState::Settled=>"settled",ProviderTransactionState::Reversed=>"reversed"}}
fn mapping_view(resource_id:ExternalResourceId,row:sqlx::postgres::PgRow)->Result<ResourceMappingView,BankingError>{Ok(ResourceMappingView{id:ResourceMappingId::new(row.get("id")),resource_id,ledger_account_id:row.get::<Option<uuid::Uuid>,_>("ledger_account_id").map(LedgerAccountId::new),mapping_version:row.get("mapping_version"),state:row.get("state"),effective_at:row.get("effective_at"),ended_at:row.get("ended_at")})}
fn database(_error: sqlx::Error) -> BankingError { BankingError::InvalidValue("banking persistence failed") }
