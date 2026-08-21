use super::dto::*;
use crate::{
    api::v2::{AuthenticatedUser, V2ApiError, V2Json},
    contexts::recurring::{
        application,
        infrastructure::{MatchAllocation, StoreError},
        public::RecurringFacade,
    },
};
use axum::{
    Json,
    extract::{Path, State},
    http::HeaderMap,
};
use serde_json::{Value, json};
use std::str::FromStr;
fn key(h: &HeaderMap) -> Result<&str, V2ApiError> {
    h.get("Idempotency-Key")
        .and_then(|v| v.to_str().ok())
        .filter(|v| !v.is_empty())
        .ok_or_else(|| V2ApiError::bad_request("missing Idempotency-Key"))
}
pub(crate) async fn list(
    State(f): State<RecurringFacade>,
    AuthenticatedUser(u): AuthenticatedUser,
) -> Result<Json<Value>, V2ApiError> {
    application::queries::subscriptions(&f, u)
        .await
        .map(|v| Json(json!({"subscriptions":v})))
        .map_err(|_| V2ApiError::internal())
}
pub(crate) async fn get(
    State(f): State<RecurringFacade>,
    AuthenticatedUser(user): AuthenticatedUser,
    Path(id): Path<uuid::Uuid>,
) -> Result<Json<Value>, V2ApiError> {
    f.store
        .get_subscription(user, id)
        .await
        .map_err(|_| V2ApiError::internal())?
        .map(|view| Json(json!(view)))
        .ok_or_else(|| V2ApiError::not_found("subscription not found"))
}
pub(crate) async fn patch(
    State(f): State<RecurringFacade>,
    AuthenticatedUser(user): AuthenticatedUser,
    Path(id): Path<uuid::Uuid>,
    h: HeaderMap,
    V2Json(b): V2Json<UpdateSubscription>,
) -> Result<Json<Value>, V2ApiError> {
    let key = key(&h)?;
    if b.expected_version == 0 {
        return Err(V2ApiError::bad_request("invalid expected_version"));
    }
    if b.status.is_none() && b.category_id.is_none() {
        return Err(V2ApiError::bad_request("subscription patch is empty"));
    }
    let hash = application::commands::canonical_request_hash(
        "update_subscription",
        &id.to_string(),
        user,
        &b,
    )
    .map_err(|_| V2ApiError::bad_request("invalid request"))?;
    f.store
        .update_subscription(
            user,
            id,
            b.expected_version,
            b.status.as_deref(),
            b.category_id,
            key,
            hash,
        )
        .await
        .map(Json)
        .map_err(map_store)
}
pub(crate) async fn charges(
    State(f): State<RecurringFacade>,
    AuthenticatedUser(user): AuthenticatedUser,
    Path(id): Path<uuid::Uuid>,
) -> Result<Json<Value>, V2ApiError> {
    f.store
        .charges(user, id)
        .await
        .map(|charges| Json(json!({"charges":charges})))
        .map_err(|_| V2ApiError::internal())
}
pub(crate) async fn forecast(
    State(f): State<RecurringFacade>,
    AuthenticatedUser(user): AuthenticatedUser,
) -> Result<Json<Value>, V2ApiError> {
    f.store
        .forecast(user)
        .await
        .map(|forecast| Json(json!({"forecast":forecast})))
        .map_err(|_| V2ApiError::internal())
}
pub(crate) async fn create_match(
    State(f): State<RecurringFacade>,
    AuthenticatedUser(user): AuthenticatedUser,
    Path(evidence_id): Path<uuid::Uuid>,
    h: HeaderMap,
    V2Json(b): V2Json<MatchBody>,
) -> Result<Json<Value>, V2ApiError> {
    let key = key(&h)?;
    if b.allocations.is_empty() {
        return Err(V2ApiError::bad_request("allocations are required"));
    }
    let allocations = b
        .allocations
        .iter()
        .map(|allocation| {
            Ok(MatchAllocation {
                journal_entry_id: allocation.journal_entry_id,
                amount: rust_decimal::Decimal::from_str(&allocation.amount)
                    .map_err(|_| V2ApiError::bad_request("invalid allocation amount"))?,
                currency: crate::shared_kernel::CurrencyCode::new(&allocation.currency)
                    .map_err(|_| V2ApiError::bad_request("invalid allocation currency"))?
                    .to_string(),
            })
        })
        .collect::<Result<Vec<_>, V2ApiError>>()?;
    let hash = application::commands::canonical_request_hash(
        "match_charge",
        &evidence_id.to_string(),
        user,
        &b,
    )
    .map_err(|_| V2ApiError::bad_request("invalid request"))?;
    f.store
        .create_match(
            user,
            evidence_id,
            b.expected_version,
            allocations,
            key,
            hash,
        )
        .await
        .map(Json)
        .map_err(map_store)
}
pub(crate) async fn reject(
    State(f): State<RecurringFacade>,
    AuthenticatedUser(user): AuthenticatedUser,
    Path(evidence_id): Path<uuid::Uuid>,
    h: HeaderMap,
    V2Json(b): V2Json<RejectionBody>,
) -> Result<Json<Value>, V2ApiError> {
    let key = key(&h)?;
    if b.reason.trim().is_empty() {
        return Err(V2ApiError::bad_request("reason is required"));
    }
    let hash = application::commands::canonical_request_hash(
        "reject_charge",
        &evidence_id.to_string(),
        user,
        &b,
    )
    .map_err(|_| V2ApiError::bad_request("invalid request"))?;
    f.store
        .reject(user, evidence_id, b.expected_version, &b.reason, key, hash)
        .await
        .map(Json)
        .map_err(map_store)
}
pub(crate) async fn unmatch(
    State(f): State<RecurringFacade>,
    AuthenticatedUser(user): AuthenticatedUser,
    Path((evidence_id, match_id)): Path<(uuid::Uuid, uuid::Uuid)>,
    h: HeaderMap,
    V2Json(b): V2Json<UnmatchBody>,
) -> Result<Json<Value>, V2ApiError> {
    let key = key(&h)?;
    let hash = application::commands::canonical_request_hash(
        "unmatch_charge",
        &format!("{evidence_id}:{match_id}"),
        user,
        &b,
    )
    .map_err(|_| V2ApiError::bad_request("invalid request"))?;
    f.store
        .unmatch(user, evidence_id, match_id, b.expected_version, key, hash)
        .await
        .map(Json)
        .map_err(map_store)
}

fn map_store(error: StoreError) -> V2ApiError {
    match error {
        StoreError::NotFound => V2ApiError::not_found("recurring item not found"),
        StoreError::VersionConflict => V2ApiError::conflict("version_conflict"),
        StoreError::IdempotencyConflict => V2ApiError::conflict("idempotency_conflict"),
        StoreError::CategorizationPending => V2ApiError::conflict("categorization_pending"),
        StoreError::Invalid(message) => V2ApiError::bad_request(message),
        StoreError::Database(_) => V2ApiError::internal(),
    }
}
