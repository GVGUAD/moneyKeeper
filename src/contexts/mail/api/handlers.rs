use super::dto::{ExpectedVersion, OauthCallbackQuery, OauthStartBody};
use crate::{
    api::{ApiError, ApiJson, AuthenticatedUser},
    contexts::mail::public::{
        ConnectionVersion, GmailConnectionId, MailFacade, MailFacadeError, StartOauth,
    },
};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode, header::LOCATION},
};
use serde_json::{Value, json};
fn require_key(headers: &HeaderMap) -> Result<&str, ApiError> {
    headers
        .get("Idempotency-Key")
        .and_then(|v| v.to_str().ok())
        .filter(|v| !v.trim().is_empty())
        .ok_or_else(|| ApiError::bad_request("missing Idempotency-Key"))
}
pub(crate) async fn oauth_start(
    State(f): State<MailFacade>,
    AuthenticatedUser(user): AuthenticatedUser,
    headers: HeaderMap,
    ApiJson(body): ApiJson<OauthStartBody>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let key = require_key(&headers)?;
    if body.connection_id.is_some() != body.expected_version.is_some() {
        return Err(ApiError::bad_request(
            "replacement requires connection_id and expected_version",
        ));
    }
    let result = f
        .start_oauth(
            user,
            StartOauth {
                replacement_connection_id: body.connection_id.map(GmailConnectionId::new),
                expected_version: body
                    .expected_version
                    .map(ConnectionVersion::new)
                    .transpose()
                    .map_err(|_| ApiError::bad_request("invalid expected_version"))?,
            },
            key,
            chrono::Utc::now(),
        )
        .await
        .map_err(map_mail)?;
    Ok((StatusCode::OK, Json(result)))
}
pub(crate) async fn callback(
    State(f): State<MailFacade>,
    Query(query): Query<OauthCallbackQuery>,
) -> Result<
    (
        StatusCode,
        [(axum::http::HeaderName, String); 1],
        Json<Value>,
    ),
    ApiError,
> {
    let result = f
        .complete_oauth_callback(&query.state, &query.code, chrono::Utc::now())
        .await
        .map_err(map_mail)?;
    let redirect = result
        .get("redirect")
        .and_then(Value::as_str)
        .unwrap_or("/settings/email?status=connected")
        .to_owned();
    Ok((StatusCode::SEE_OTHER, [(LOCATION, redirect)], Json(result)))
}

fn map_mail(error: MailFacadeError) -> ApiError {
    if error.is_not_found() {
        ApiError::not_found("email connection not found")
    } else if error.is_idempotency_conflict() {
        ApiError::conflict("idempotency_conflict")
    } else if error.is_conflict() {
        ApiError::conflict("version_conflict")
    } else if error.is_invalid() {
        ApiError::bad_request("invalid_oauth_state")
    } else if error.is_oauth_provider() {
        ApiError::bad_gateway(
            "oauth_provider_failed",
            "mail.oauth_provider",
            "OAuth provider request failed",
        )
    } else {
        ApiError::internal("mail.persistence", "mail storage operation failed")
    }
}
pub(crate) async fn list(
    State(f): State<MailFacade>,
    AuthenticatedUser(user): AuthenticatedUser,
) -> Result<Json<Value>, ApiError> {
    f.list_connections(user)
        .await
        .map(|v| Json(json!({"connections":v})))
        .map_err(|_| ApiError::internal("mail.persistence", "mail connection query failed"))
}
pub(crate) async fn status(
    State(f): State<MailFacade>,
    AuthenticatedUser(user): AuthenticatedUser,
    Path(id): Path<uuid::Uuid>,
) -> Result<Json<Value>, ApiError> {
    f.connection_status(user, GmailConnectionId::new(id))
        .await
        .map_err(|_| ApiError::internal("mail.persistence", "mail connection query failed"))?
        .map(Json)
        .ok_or_else(|| ApiError::not_found("email connection not found"))
}
pub(crate) async fn disconnect(
    State(f): State<MailFacade>,
    AuthenticatedUser(user): AuthenticatedUser,
    Path(id): Path<uuid::Uuid>,
    headers: HeaderMap,
    ApiJson(body): ApiJson<ExpectedVersion>,
) -> Result<Json<Value>, ApiError> {
    let key = require_key(&headers)?;
    let version = ConnectionVersion::new(body.expected_version)
        .map_err(|_| ApiError::bad_request("invalid expected_version"))?;
    f.disconnect(
        user,
        GmailConnectionId::new(id),
        version,
        key,
        chrono::Utc::now(),
    )
    .await
    .map(Json)
    .map_err(map_mail)
}
pub(crate) async fn resync(
    State(f): State<MailFacade>,
    AuthenticatedUser(user): AuthenticatedUser,
    Path(id): Path<uuid::Uuid>,
    headers: HeaderMap,
    ApiJson(body): ApiJson<ExpectedVersion>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let key = require_key(&headers)?;
    let version = ConnectionVersion::new(body.expected_version)
        .map_err(|_| ApiError::bad_request("invalid expected_version"))?;
    f.resync(
        user,
        GmailConnectionId::new(id),
        version,
        key,
        chrono::Utc::now(),
    )
    .await
    .map(|response| (StatusCode::ACCEPTED, Json(response)))
    .map_err(map_mail)
}
