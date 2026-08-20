//! PostgreSQL Banking store.

use std::collections::BTreeMap;

use serde_json::json;
use sha2::{Digest, Sha256};
use sqlx::Row;

use crate::{contexts::banking::{application::{ConnectProvider, ConnectionResult, CredentialBinding, CredentialCipher, ProviderClient, ProviderConnectionView, ReplaceProviderCredential}, domain::{BankingError, ConnectionState, ConnectionVersion, CredentialEnvelope, ProviderConnectionId}}, infrastructure::v2_db::VerifiedV2Pool, shared_kernel::{CurrencyCode, UserId}};

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

    pub(crate) async fn validate_and_discover(&self, user_id: UserId, connection_id: ProviderConnectionId, cipher: &dyn CredentialCipher, provider_client: &dyn ProviderClient) -> Result<Vec<NormalizedResource>, BankingError> {
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
        let currency_rows = sqlx::query("SELECT numeric_code,code,minor_unit FROM reference_data.currencies WHERE enabled AND numeric_code IS NOT NULL").fetch_all(&self.uow.pool).await.map_err(database)?;
        let currencies = currency_rows.into_iter().map(|row| { let numeric: String=row.get("numeric_code"); let code: String=row.get("code"); (numeric.parse::<u16>().unwrap(), (CurrencyCode::new(code).unwrap(), u8::try_from(row.get::<i16,_>("minor_unit")).unwrap())) }).collect::<BTreeMap<_,_>>();
        let snapshot = MonobankAdapter::normalize_client_info(&body, &currencies)?;
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

    async fn connection_row(&self, user_id: UserId, connection_id: ProviderConnectionId) -> Result<ConnectionRow, BankingError> {
        let row = sqlx::query("SELECT provider,state,active_credential_ciphertext,active_credential_nonce,active_credential_key_id,active_credential_envelope_version,pending_credential_ciphertext,pending_credential_nonce,pending_credential_key_id,pending_credential_envelope_version,credential_generation FROM banking.provider_connections WHERE id=$1 AND user_id=$2")
            .bind(connection_id.into_uuid()).bind(user_id.into_uuid()).fetch_optional(&self.uow.pool).await.map_err(database)?.ok_or(BankingError::InvalidState)?;
        Ok(ConnectionRow { provider: row.get("provider"), state: row.get("state"), active_credential_ciphertext: row.get("active_credential_ciphertext"), active_credential_nonce: row.get("active_credential_nonce"), active_credential_key_id: row.get("active_credential_key_id"), active_credential_envelope_version: row.get("active_credential_envelope_version"), pending_credential_ciphertext: row.get("pending_credential_ciphertext"), pending_credential_nonce: row.get("pending_credential_nonce"), pending_credential_key_id: row.get("pending_credential_key_id"), pending_credential_envelope_version: row.get("pending_credential_envelope_version"), credential_generation: row.get("credential_generation") })
    }
}

fn view(row: sqlx::postgres::PgRow) -> Result<ProviderConnectionView, BankingError> {
    Ok(ProviderConnectionView { id: ProviderConnectionId::new(row.get("id")), user_id: UserId::new(row.get("user_id")), provider: row.get("provider"), state: state(row.get::<String,_>("state").as_str())?, credential_generation: row.get("credential_generation"), version: ConnectionVersion::new(row.get("version"))?, webhook_configured: row.get::<Option<Vec<u8>>,_>("webhook_lookup_digest").is_some(), created_at: row.get("created_at"), updated_at: row.get("updated_at") })
}
fn state(value: &str) -> Result<ConnectionState, BankingError> { match value { "pending"=>Ok(ConnectionState::Pending),"active"=>Ok(ConnectionState::Active),"pending_credential_validation"=>Ok(ConnectionState::PendingCredentialValidation),"needs_reauth"=>Ok(ConnectionState::NeedsReauth),"revoked"=>Ok(ConnectionState::Revoked),_=>Err(BankingError::InvalidValue("stored connection state is invalid")) } }
fn kind(value: crate::contexts::banking::domain::ResourceKind) -> &'static str { match value { crate::contexts::banking::domain::ResourceKind::Card=>"card",crate::contexts::banking::domain::ResourceKind::CurrentAccount=>"current_account",crate::contexts::banking::domain::ResourceKind::Jar=>"jar",crate::contexts::banking::domain::ResourceKind::SecurityPortfolio=>"security_portfolio",crate::contexts::banking::domain::ResourceKind::Unsupported=>"unsupported" } }
fn funding(value: crate::contexts::banking::domain::FundingModel) -> &'static str { match value { crate::contexts::banking::domain::FundingModel::OwnFunds=>"own_funds",crate::contexts::banking::domain::FundingModel::RevolvingCredit=>"revolving_credit",crate::contexts::banking::domain::FundingModel::Unknown=>"unknown" } }
fn database(_error: sqlx::Error) -> BankingError { BankingError::InvalidValue("banking persistence failed") }
