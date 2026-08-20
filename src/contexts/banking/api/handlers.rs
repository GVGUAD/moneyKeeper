use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
};
use chrono::Utc;
use uuid::Uuid;

use super::dto::*;
use crate::{
    api::v2::{AuthenticatedUser, V2ApiError, V2Json},
    contexts::{banking::public::*, ledger::public::LedgerAccountId},
    shared_kernel::{CorrelationId, IdempotencyKey},
};

pub(crate) async fn connect(
    AuthenticatedUser(user): AuthenticatedUser,
    State(banking): State<BankingFacade>,
    headers: HeaderMap,
    V2Json(request): V2Json<ConnectRequest>,
) -> Result<(StatusCode, Json<ConnectionResult>), V2ApiError> {
    let result = banking
        .connect_provider(ConnectProvider {
            user_id: user,
            provider: "monobank".to_owned(),
            credential: ProviderCredential::new(request.x_token).map_err(map)?,
            idempotency_key: key(&headers)?,
            correlation_id: CorrelationId::generate(),
            requested_at: Utc::now(),
        })
        .await
        .map_err(map)?;
    Ok((StatusCode::ACCEPTED, Json(result)))
}
pub(crate) async fn list_connections(
    AuthenticatedUser(user): AuthenticatedUser,
    State(banking): State<BankingFacade>,
) -> Result<Json<Vec<ProviderConnectionView>>, V2ApiError> {
    banking.list_connections(user).await.map(Json).map_err(map)
}
pub(crate) async fn get_connection(
    AuthenticatedUser(user): AuthenticatedUser,
    State(banking): State<BankingFacade>,
    Path(id): Path<Uuid>,
) -> Result<Json<ProviderConnectionView>, V2ApiError> {
    banking
        .get_connection(user, ProviderConnectionId::new(id))
        .await
        .map(Json)
        .map_err(map)
}
pub(crate) async fn disconnect(
    AuthenticatedUser(user): AuthenticatedUser,
    State(banking): State<BankingFacade>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    V2Json(request): V2Json<ExpectedVersionRequest>,
) -> Result<Json<ProviderConnectionView>, V2ApiError> {
    key(&headers)?;
    banking
        .disconnect(
            user,
            ProviderConnectionId::new(id),
            ConnectionVersion::new(request.expected_version).map_err(map)?,
            Utc::now(),
        )
        .await
        .map(Json)
        .map_err(map)
}
pub(crate) async fn replace_credential(
    AuthenticatedUser(user): AuthenticatedUser,
    State(banking): State<BankingFacade>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    V2Json(request): V2Json<ReplaceCredentialRequest>,
) -> Result<(StatusCode, Json<ConnectionResult>), V2ApiError> {
    let result = banking
        .replace_provider_credential(ReplaceProviderCredential {
            user_id: user,
            connection_id: ProviderConnectionId::new(id),
            credential: ProviderCredential::new(request.x_token).map_err(map)?,
            expected_version: ConnectionVersion::new(request.expected_version).map_err(map)?,
            idempotency_key: key(&headers)?,
            correlation_id: CorrelationId::generate(),
            requested_at: Utc::now(),
        })
        .await
        .map_err(map)?;
    Ok((StatusCode::ACCEPTED, Json(result)))
}
pub(crate) async fn rotate_webhook(
    AuthenticatedUser(user): AuthenticatedUser,
    State(banking): State<BankingFacade>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    V2Json(request): V2Json<ExpectedVersionRequest>,
) -> Result<(StatusCode, Json<serde_json::Value>), V2ApiError> {
    key(&headers)?;
    let result = banking
        .rotate_webhook_credential(RotateWebhookCredential {
            user_id: user,
            connection_id: ProviderConnectionId::new(id),
            expected_version: ConnectionVersion::new(request.expected_version).map_err(map)?,
            requested_at: Utc::now(),
        })
        .await
        .map_err(map)?;
    Ok((
        StatusCode::CREATED,
        Json(
            serde_json::json!({"connection_id":result.connection_id,"webhook_credential":result.credential.expose(),"desired_version":result.desired_version,"connection_version":result.connection_version}),
        ),
    ))
}
pub(crate) async fn list_resources(
    AuthenticatedUser(user): AuthenticatedUser,
    State(banking): State<BankingFacade>,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<ExternalResourceView>>, V2ApiError> {
    banking
        .list_resources(user, ProviderConnectionId::new(id))
        .await
        .map(Json)
        .map_err(map)
}
pub(crate) async fn map_resource(
    AuthenticatedUser(user): AuthenticatedUser,
    State(banking): State<BankingFacade>,
    Path(_connection): Path<Uuid>,
    headers: HeaderMap,
    V2Json(request): V2Json<MappingRequest>,
) -> Result<(StatusCode, Json<ResourceMappingResult>), V2ApiError> {
    let resource = ExternalResourceId::new(request.resource_id);
    let result = if let Some(account) = request.ledger_account_id {
        banking
            .bind_existing_resource(BindExistingResource {
                user_id: user,
                resource_id: resource,
                ledger_account_id: LedgerAccountId::new(account),
                expected_resource_version: request.expected_version,
                idempotency_key: key(&headers)?,
                correlation_id: CorrelationId::generate(),
                requested_at: Utc::now(),
            })
            .await
    } else {
        banking
            .create_and_map_resource(CreateAndMapResource {
                user_id: user,
                resource_id: resource,
                account_name: request
                    .account_name
                    .ok_or_else(|| V2ApiError::bad_request("account_name is required"))?,
                expected_resource_version: request.expected_version,
                idempotency_key: key(&headers)?,
                correlation_id: CorrelationId::generate(),
                requested_at: Utc::now(),
            })
            .await
    }
    .map_err(map)?;
    Ok((StatusCode::ACCEPTED, Json(result)))
}
pub(crate) async fn deactivate_mapping(
    AuthenticatedUser(user): AuthenticatedUser,
    State(banking): State<BankingFacade>,
    Path((_connection, _mapping)): Path<(Uuid, Uuid)>,
    headers: HeaderMap,
    V2Json(request): V2Json<MappingChangeRequest>,
) -> Result<Json<ResourceMappingResult>, V2ApiError> {
    banking
        .deactivate_resource_mapping(DeactivateResourceMapping {
            user_id: user,
            resource_id: ExternalResourceId::new(request.resource_id),
            expected_resource_version: request.expected_version,
            reason: request.reason,
            idempotency_key: key(&headers)?,
            correlation_id: CorrelationId::generate(),
            requested_at: Utc::now(),
        })
        .await
        .map(Json)
        .map_err(map)
}
pub(crate) async fn replace_mapping(
    AuthenticatedUser(user): AuthenticatedUser,
    State(banking): State<BankingFacade>,
    Path((_connection, _mapping)): Path<(Uuid, Uuid)>,
    headers: HeaderMap,
    V2Json(request): V2Json<MappingChangeRequest>,
) -> Result<(StatusCode, Json<ResourceMappingResult>), V2ApiError> {
    let resource = ExternalResourceId::new(request.resource_id);
    banking
        .deactivate_resource_mapping(DeactivateResourceMapping {
            user_id: user,
            resource_id: resource,
            expected_resource_version: request.expected_version,
            reason: request.reason,
            idempotency_key: key(&headers)?,
            correlation_id: CorrelationId::generate(),
            requested_at: Utc::now(),
        })
        .await
        .map_err(map)?;
    let result = if let Some(account) = request.ledger_account_id {
        banking
            .bind_existing_resource(BindExistingResource {
                user_id: user,
                resource_id: resource,
                ledger_account_id: LedgerAccountId::new(account),
                expected_resource_version: request.expected_version + 1,
                idempotency_key: key(&headers)?,
                correlation_id: CorrelationId::generate(),
                requested_at: Utc::now(),
            })
            .await
    } else {
        banking
            .create_and_map_resource(CreateAndMapResource {
                user_id: user,
                resource_id: resource,
                account_name: request
                    .account_name
                    .ok_or_else(|| V2ApiError::bad_request("account_name is required"))?,
                expected_resource_version: request.expected_version + 1,
                idempotency_key: key(&headers)?,
                correlation_id: CorrelationId::generate(),
                requested_at: Utc::now(),
            })
            .await
    }
    .map_err(map)?;
    Ok((StatusCode::ACCEPTED, Json(result)))
}
pub(crate) async fn request_sync(
    AuthenticatedUser(user): AuthenticatedUser,
    State(banking): State<BankingFacade>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    V2Json(request): V2Json<SyncRequest>,
) -> Result<(StatusCode, Json<SyncJobView>), V2ApiError> {
    let result = banking
        .request_sync_job(RequestSyncJob {
            user_id: user,
            connection_id: ProviderConnectionId::new(id),
            requested_from: request.requested_from,
            requested_to: request.requested_to,
            overlap_seconds: request.overlap_seconds,
            idempotency_key: key(&headers)?,
            correlation_id: CorrelationId::generate(),
        })
        .await
        .map_err(map)?;
    Ok((StatusCode::ACCEPTED, Json(result)))
}
pub(crate) async fn get_sync(
    AuthenticatedUser(user): AuthenticatedUser,
    State(banking): State<BankingFacade>,
    Path(id): Path<Uuid>,
) -> Result<Json<SyncJobView>, V2ApiError> {
    banking
        .get_sync_job(user, SyncJobId::new(id))
        .await
        .map(Json)
        .map_err(map)
}
pub(crate) async fn get_event(
    AuthenticatedUser(user): AuthenticatedUser,
    State(banking): State<BankingFacade>,
    Path(id): Path<Uuid>,
) -> Result<Json<ProviderEventView>, V2ApiError> {
    banking
        .get_provider_event(user, ProviderEventId::new(id))
        .await
        .map(Json)
        .map_err(map)
}
pub(crate) async fn get_process(
    AuthenticatedUser(user): AuthenticatedUser,
    State(banking): State<BankingFacade>,
    Path(id): Path<Uuid>,
) -> Result<Json<AccountingProcessView>, V2ApiError> {
    banking
        .get_accounting_process(user, id)
        .await
        .map(Json)
        .map_err(map)
}
pub(crate) async fn get_observation(
    AuthenticatedUser(user): AuthenticatedUser,
    State(banking): State<BankingFacade>,
    Path(id): Path<Uuid>,
) -> Result<Json<BalanceObservationView>, V2ApiError> {
    banking
        .get_balance_observation(user, BalanceObservationId::new(id))
        .await
        .map(Json)
        .map_err(map)
}
fn key(headers: &HeaderMap) -> Result<IdempotencyKey, V2ApiError> {
    let value = headers
        .get("Idempotency-Key")
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| V2ApiError::bad_request("Idempotency-Key is required"))?;
    IdempotencyKey::new(value).map_err(|_| V2ApiError::bad_request("invalid Idempotency-Key"))
}
fn map(error: BankingError) -> V2ApiError {
    if matches!(error, BankingError::IdempotencyConflict) {
        V2ApiError::conflict("idempotency key conflicts with an earlier request")
    } else if matches!(
        error,
        BankingError::VersionConflict | BankingError::MappingAlreadyActive
    ) {
        V2ApiError::conflict("banking version conflict")
    } else if matches!(
        error,
        BankingError::InvalidState | BankingError::MappingNotActive
    ) {
        V2ApiError::not_found("banking resource was not found")
    } else {
        V2ApiError::bad_request("banking request was rejected")
    }
}
