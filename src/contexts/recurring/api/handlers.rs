use crate::{
    api::{ApiError, ApiJson, AuthenticatedUser},
    contexts::recurring::public::{
        ChargeEvidenceId, MatchCharge, MatchId, RecurringFacade, RecurringFacadeError,
        RejectCharge, SubscriptionId, UnmatchCharge, UpdateSubscription,
    },
};
use axum::{
    Json,
    extract::{Path, State},
    http::HeaderMap,
};
use serde_json::{Value, json};
fn key(h: &HeaderMap) -> Result<&str, ApiError> {
    h.get("Idempotency-Key")
        .and_then(|v| v.to_str().ok())
        .filter(|v| !v.is_empty())
        .ok_or_else(|| ApiError::bad_request("missing Idempotency-Key"))
}
pub(crate) async fn list(
    State(f): State<RecurringFacade>,
    AuthenticatedUser(u): AuthenticatedUser,
) -> Result<Json<Value>, ApiError> {
    f.subscriptions(u)
        .await
        .map(|v| Json(json!({"subscriptions":v})))
        .map_err(|_| ApiError::internal())
}
pub(crate) async fn get(
    State(f): State<RecurringFacade>,
    AuthenticatedUser(user): AuthenticatedUser,
    Path(id): Path<uuid::Uuid>,
) -> Result<Json<Value>, ApiError> {
    f.subscription(user, SubscriptionId::new(id))
        .await
        .map_err(|_| ApiError::internal())?
        .map(|view| Json(json!(view)))
        .ok_or_else(|| ApiError::not_found("subscription not found"))
}
pub(crate) async fn patch(
    State(f): State<RecurringFacade>,
    AuthenticatedUser(user): AuthenticatedUser,
    Path(id): Path<uuid::Uuid>,
    h: HeaderMap,
    ApiJson(b): ApiJson<UpdateSubscription>,
) -> Result<Json<Value>, ApiError> {
    let key = key(&h)?;
    f.update_subscription(user, SubscriptionId::new(id), b, key)
        .await
        .map(Json)
        .map_err(map_store)
}
pub(crate) async fn charges(
    State(f): State<RecurringFacade>,
    AuthenticatedUser(user): AuthenticatedUser,
    Path(id): Path<uuid::Uuid>,
) -> Result<Json<Value>, ApiError> {
    f.charges(user, SubscriptionId::new(id))
        .await
        .map(|charges| Json(json!({"charges":charges})))
        .map_err(|_| ApiError::internal())
}
pub(crate) async fn forecast(
    State(f): State<RecurringFacade>,
    AuthenticatedUser(user): AuthenticatedUser,
) -> Result<Json<Value>, ApiError> {
    f.forecast(user)
        .await
        .map(|forecast| Json(json!({"forecast":forecast})))
        .map_err(|_| ApiError::internal())
}
pub(crate) async fn create_match(
    State(f): State<RecurringFacade>,
    AuthenticatedUser(user): AuthenticatedUser,
    Path(evidence_id): Path<uuid::Uuid>,
    h: HeaderMap,
    ApiJson(b): ApiJson<MatchCharge>,
) -> Result<Json<Value>, ApiError> {
    let key = key(&h)?;
    f.match_charge(user, ChargeEvidenceId::new(evidence_id), b, key)
        .await
        .map(Json)
        .map_err(map_store)
}
pub(crate) async fn reject(
    State(f): State<RecurringFacade>,
    AuthenticatedUser(user): AuthenticatedUser,
    Path(evidence_id): Path<uuid::Uuid>,
    h: HeaderMap,
    ApiJson(b): ApiJson<RejectCharge>,
) -> Result<Json<Value>, ApiError> {
    let key = key(&h)?;
    f.reject_charge(user, ChargeEvidenceId::new(evidence_id), b, key)
        .await
        .map(Json)
        .map_err(map_store)
}
pub(crate) async fn unmatch(
    State(f): State<RecurringFacade>,
    AuthenticatedUser(user): AuthenticatedUser,
    Path((evidence_id, match_id)): Path<(uuid::Uuid, uuid::Uuid)>,
    h: HeaderMap,
    ApiJson(b): ApiJson<UnmatchCharge>,
) -> Result<Json<Value>, ApiError> {
    let key = key(&h)?;
    f.unmatch_charge(
        user,
        ChargeEvidenceId::new(evidence_id),
        MatchId::new(match_id),
        b,
        key,
    )
    .await
    .map(Json)
    .map_err(map_store)
}

fn map_store(error: RecurringFacadeError) -> ApiError {
    if error.is_not_found() {
        ApiError::not_found("recurring item not found")
    } else if error.is_version_conflict() {
        ApiError::conflict("version_conflict")
    } else if error.is_idempotency_conflict() {
        ApiError::conflict("idempotency_conflict")
    } else if error.is_categorization_pending() {
        ApiError::conflict("categorization_pending")
    } else if let Some(message) = error.invalid_reason() {
        ApiError::bad_request(message)
    } else {
        ApiError::internal()
    }
}
