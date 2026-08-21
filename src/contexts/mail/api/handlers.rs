use super::dto::{ExpectedVersion, OauthCallbackQuery, OauthStartBody};
use crate::{
    api::v2::{AuthenticatedUser, V2ApiError, V2Json},
    contexts::mail::{
        application,
        domain::{ConnectionVersion, GmailConnectionId},
        infrastructure::MailStoreError,
        public::MailFacade,
    },
};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode, header::LOCATION},
};
use serde_json::{Value, json};
fn require_key(headers: &HeaderMap) -> Result<&str, V2ApiError> {
    headers
        .get("Idempotency-Key")
        .and_then(|v| v.to_str().ok())
        .filter(|v| !v.trim().is_empty())
        .ok_or_else(|| V2ApiError::bad_request("missing Idempotency-Key"))
}
pub(crate) async fn oauth_start(
    State(f): State<MailFacade>,
    AuthenticatedUser(user): AuthenticatedUser,
    headers: HeaderMap,
    V2Json(body): V2Json<OauthStartBody>,
) -> Result<(StatusCode, Json<Value>), V2ApiError> {
    let key = require_key(&headers)?;
    if body.connection_id.is_some() != body.expected_version.is_some() {
        return Err(V2ApiError::bad_request(
            "replacement requires connection_id and expected_version",
        ));
    }
    let hash = application::commands::canonical_request_hash(
        "gmail_oauth_start",
        body.connection_id
            .as_ref()
            .map(uuid::Uuid::to_string)
            .as_deref(),
        user,
        &body,
    )
    .map_err(|_| V2ApiError::bad_request("invalid request"))?;
    let result = f
        .store
        .start_oauth(
            user,
            body.connection_id,
            body.expected_version,
            key,
            hash,
            chrono::Utc::now(),
            f.oauth.as_ref(),
        )
        .await
        .map_err(map_store)?;
    Ok((StatusCode::OK, Json(result.response)))
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
    V2ApiError,
> {
    let preparation = f
        .store
        .prepare_oauth_callback(&query.state, &query.code, chrono::Utc::now())
        .await
        .map_err(map_store)?;
    let result = match preparation {
        crate::contexts::mail::infrastructure::OauthCallbackPreparation::Replay(result) => result,
        crate::contexts::mail::infrastructure::OauthCallbackPreparation::Exchange { verifier } => {
            let tokens = match f.oauth.exchange(&query.code, &verifier).await {
                Ok(tokens) => tokens,
                Err(_) => {
                    let _ = f
                        .store
                        .record_oauth_provider_failure(
                            &query.state,
                            &query.code,
                            chrono::Utc::now(),
                        )
                        .await;
                    return Err(V2ApiError::bad_gateway("oauth_provider_failed"));
                }
            };
            f.store
                .complete_oauth(&query.state, &query.code, tokens, chrono::Utc::now())
                .await
                .map_err(map_store)?
        }
    };
    let redirect = result
        .response
        .get("redirect")
        .and_then(Value::as_str)
        .unwrap_or("/settings/email?status=connected")
        .to_owned();
    Ok((
        StatusCode::SEE_OTHER,
        [(LOCATION, redirect)],
        Json(result.response),
    ))
}

fn map_store(error: MailStoreError) -> V2ApiError {
    match error {
        MailStoreError::NotFound => V2ApiError::not_found("email connection not found"),
        MailStoreError::VersionConflict => V2ApiError::conflict("version_conflict"),
        MailStoreError::IdempotencyConflict => V2ApiError::conflict("idempotency_conflict"),
        MailStoreError::InvalidOauthState => V2ApiError::bad_request("invalid_oauth_state"),
        MailStoreError::OAuthProvider => V2ApiError::bad_gateway("oauth_provider_failed"),
        MailStoreError::Database(_) => V2ApiError::internal(),
    }
}
pub(crate) async fn list(
    State(f): State<MailFacade>,
    AuthenticatedUser(user): AuthenticatedUser,
) -> Result<Json<Value>, V2ApiError> {
    application::queries::list(&f, user)
        .await
        .map(|v| Json(json!({"connections":v})))
        .map_err(|_| V2ApiError::internal())
}
pub(crate) async fn status(
    State(f): State<MailFacade>,
    AuthenticatedUser(user): AuthenticatedUser,
    Path(id): Path<uuid::Uuid>,
) -> Result<Json<Value>, V2ApiError> {
    f.store
        .connection_status(user, GmailConnectionId::new(id))
        .await
        .map_err(|_| V2ApiError::internal())?
        .map(Json)
        .ok_or_else(|| V2ApiError::not_found("email connection not found"))
}
pub(crate) async fn disconnect(
    State(f): State<MailFacade>,
    AuthenticatedUser(user): AuthenticatedUser,
    Path(id): Path<uuid::Uuid>,
    headers: HeaderMap,
    V2Json(body): V2Json<ExpectedVersion>,
) -> Result<Json<Value>, V2ApiError> {
    let key = require_key(&headers)?;
    let version = ConnectionVersion::new(body.expected_version)
        .map_err(|_| V2ApiError::bad_request("invalid expected_version"))?;
    let hash = application::commands::canonical_request_hash(
        "disconnect_email_connection",
        Some(&id.to_string()),
        user,
        &body,
    )
    .map_err(|_| V2ApiError::bad_request("invalid request"))?;
    f.store
        .disconnect_command(user, id, version.get(), key, hash, chrono::Utc::now())
        .await
        .map(Json)
        .map_err(map_store)
}
pub(crate) async fn resync(
    State(f): State<MailFacade>,
    AuthenticatedUser(user): AuthenticatedUser,
    Path(id): Path<uuid::Uuid>,
    headers: HeaderMap,
    V2Json(body): V2Json<ExpectedVersion>,
) -> Result<(StatusCode, Json<Value>), V2ApiError> {
    let key = require_key(&headers)?;
    let version = ConnectionVersion::new(body.expected_version)
        .map_err(|_| V2ApiError::bad_request("invalid expected_version"))?;
    let hash = application::commands::canonical_request_hash(
        "resync_email_connection",
        Some(&id.to_string()),
        user,
        &body,
    )
    .map_err(|_| V2ApiError::bad_request("invalid request"))?;
    f.store
        .resync_command(user, id, version.get(), key, hash, chrono::Utc::now())
        .await
        .map(|response| (StatusCode::ACCEPTED, Json(response)))
        .map_err(map_store)
}
