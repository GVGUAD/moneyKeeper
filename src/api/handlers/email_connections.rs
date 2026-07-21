use std::sync::Arc;

use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::{
    Extension, Json,
    extract::{Path, Query, State, rejection::JsonRejection},
    http::StatusCode,
};
use serde::Deserialize;
use uuid::Uuid;

use crate::api::{
    dto::{EmailConnectionResponse, GmailOAuthCallbackRequest, GmailOAuthStartResponse},
    error::AppError,
    middleware::AuthUser,
    state::AppState,
};
use crate::domain::email_connection::EmailConnection;
use crate::infrastructure::email::oauth::{GmailProviderError, OAuthFlowError};

fn to_response(c: EmailConnection) -> EmailConnectionResponse {
    EmailConnectionResponse {
        id: c.id,
        email_address: c.email_address,
        provider: c.provider.as_str().to_string(),
        status: c.status.as_str().to_string(),
        last_synced_at: c.last_synced_at,
        created_at: c.created_at,
    }
}

pub async fn oauth_start(
    State(state): State<AppState>,
    Extension(AuthUser(user_id)): Extension<AuthUser>,
) -> Result<Json<GmailOAuthStartResponse>, AppError> {
    let start = state.gmail_oauth.start(user_id).await?;
    Ok(Json(GmailOAuthStartResponse {
        authorize_url: start.authorize_url,
        state: start.state,
    }))
}

/// Authenticated compatibility callback for clients that receive Google's GET
/// redirect in their own frontend and forward `{code,state}` to the API.
pub async fn oauth_callback_post(
    State(state): State<AppState>,
    Extension(AuthUser(user_id)): Extension<AuthUser>,
    request: Result<Json<GmailOAuthCallbackRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<EmailConnectionResponse>), AppError> {
    let Json(req) = request.map_err(|_| {
        crate::domain::error::DomainError::InvalidInput("invalid OAuth callback body".into())
    })?;
    let connection = state
        .gmail_oauth
        .complete(&req.code, &req.state, Some(user_id))
        .await?;
    Ok((StatusCode::CREATED, Json(to_response(connection))))
}

#[derive(Debug, Deserialize)]
pub struct GmailOAuthCallbackQuery {
    pub code: Option<String>,
    pub state: Option<String>,
    pub error: Option<String>,
}

/// Public browser redirect. The high-entropy, one-time, hashed state is the
/// callback authority; no bearer token is expected on Google's redirect.
pub async fn oauth_callback_get(
    State(state): State<AppState>,
    Query(query): Query<GmailOAuthCallbackQuery>,
) -> Response {
    if query.error.is_some() {
        let Some(oauth_state) = query.state.as_deref() else {
            return callback_failure(&state, "incomplete_callback", StatusCode::BAD_REQUEST);
        };
        return match state.gmail_oauth.consume_denied_state(oauth_state).await {
            Ok(()) => callback_failure(&state, "provider_denied", StatusCode::BAD_REQUEST),
            Err(error) => {
                tracing::warn!(error = %error, "Gmail OAuth denial state validation failed");
                callback_failure(&state, "invalid_state", callback_error_status(&error))
            }
        };
    }
    let (Some(code), Some(oauth_state)) = (query.code.as_deref(), query.state.as_deref()) else {
        return callback_failure(&state, "incomplete_callback", StatusCode::BAD_REQUEST);
    };
    match state.gmail_oauth.complete(code, oauth_state, None).await {
        Ok(connection) => {
            if let Some(target) = state.gmail_oauth.success_redirect(connection.id) {
                Redirect::to(&target).into_response()
            } else {
                (
                    StatusCode::OK,
                    Html("Gmail connection completed. You may close this window."),
                )
                    .into_response()
            }
        }
        Err(error) => {
            tracing::warn!(error = %error, "Gmail OAuth callback failed");
            let status = callback_error_status(&error);
            callback_failure(&state, "callback_failed", status)
        }
    }
}

fn callback_error_status(error: &anyhow::Error) -> StatusCode {
    if let Some(provider) = error.downcast_ref::<GmailProviderError>() {
        return match provider {
            GmailProviderError::Transient => StatusCode::SERVICE_UNAVAILABLE,
            GmailProviderError::InvalidCredentials | GmailProviderError::Rejected => {
                StatusCode::BAD_GATEWAY
            }
        };
    }
    if let Some(flow) = error.downcast_ref::<OAuthFlowError>() {
        return match flow {
            OAuthFlowError::InvalidState | OAuthFlowError::IncompleteRequest => {
                StatusCode::BAD_REQUEST
            }
            OAuthFlowError::MissingRefreshToken | OAuthFlowError::InvalidProviderCredentials => {
                StatusCode::BAD_GATEWAY
            }
        };
    }
    StatusCode::INTERNAL_SERVER_ERROR
}

fn callback_failure(state: &AppState, code: &str, status: StatusCode) -> Response {
    if let Some(target) = state.gmail_oauth.failure_redirect(code) {
        Redirect::to(&target).into_response()
    } else {
        (
            status,
            Html("Gmail connection failed. Return to the application and try again."),
        )
            .into_response()
    }
}

pub async fn list(
    State(state): State<AppState>,
    Extension(AuthUser(user_id)): Extension<AuthUser>,
) -> Result<Json<Vec<EmailConnectionResponse>>, AppError> {
    let conns = state.subscriptions.list_connections(user_id).await?;
    Ok(Json(conns.into_iter().map(to_response).collect()))
}

pub async fn resync(
    State(state): State<AppState>,
    Extension(AuthUser(user_id)): Extension<AuthUser>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, AppError> {
    let job = state
        .subscriptions
        .claim_connection_for_user(id, user_id)
        .await?;
    let subscriptions = Arc::clone(&state.subscriptions);
    let matcher = Arc::clone(&state.matcher);
    tokio::spawn(async move {
        match subscriptions.run_claimed_connection(job).await {
            Ok(_) => {
                if let Err(error) = matcher.run_for_user(user_id).await {
                    tracing::warn!(%user_id, ?error, "manual email resync matching failed");
                }
            }
            Err(error) => {
                tracing::warn!(%user_id, connection_id = %id, ?error, "manual email resync failed");
            }
        }
    });
    Ok(StatusCode::ACCEPTED)
}

pub async fn delete(
    State(state): State<AppState>,
    Extension(AuthUser(user_id)): Extension<AuthUser>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, AppError> {
    state.gmail_oauth.disconnect(id, user_id).await?;
    Ok(StatusCode::NO_CONTENT)
}
