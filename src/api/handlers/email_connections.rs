use axum::{
    Extension, Json,
    extract::{Path, State},
    http::StatusCode,
};
use uuid::Uuid;

use crate::api::{
    dto::{EmailConnectionResponse, GmailOAuthCallbackRequest},
    error::AppError,
    middleware::AuthUser,
    state::AppState,
};
use crate::application::subscriptions::ConnectGmailParams;
use crate::domain::email_connection::EmailConnection;
use crate::domain::subscription_error::SubscriptionError;

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
) -> Result<Json<serde_json::Value>, AppError> {
    let oauth_state = format!("{user_id}:{}", Uuid::new_v4());
    let url = format!(
        "https://accounts.google.com/o/oauth2/v2/auth?\
         client_id={}&redirect_uri={}&response_type=code&\
         scope=https%3A%2F%2Fwww.googleapis.com%2Fauth%2Fgmail.readonly%20\
         https%3A%2F%2Fwww.googleapis.com%2Fauth%2Fuserinfo.email&\
         access_type=offline&prompt=consent&state={}",
        urlencoding::encode(&state.gmail_oauth.client_id),
        urlencoding::encode(&state.gmail_oauth.redirect_uri),
        urlencoding::encode(&oauth_state),
    );
    Ok(Json(
        serde_json::json!({ "authorize_url": url, "state": oauth_state }),
    ))
}

pub async fn oauth_callback(
    State(state): State<AppState>,
    Extension(AuthUser(user_id)): Extension<AuthUser>,
    Json(req): Json<GmailOAuthCallbackRequest>,
) -> Result<(StatusCode, Json<EmailConnectionResponse>), AppError> {
    let http = reqwest::Client::new();
    let token_resp: serde_json::Value = http
        .post("https://oauth2.googleapis.com/token")
        .form(&[
            ("client_id", state.gmail_oauth.client_id.as_str()),
            ("client_secret", state.gmail_oauth.client_secret.as_str()),
            ("code", req.code.as_str()),
            ("grant_type", "authorization_code"),
            ("redirect_uri", state.gmail_oauth.redirect_uri.as_str()),
        ])
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let access_token = token_resp["access_token"]
        .as_str()
        .unwrap_or("")
        .to_string();
    let refresh_token = token_resp["refresh_token"]
        .as_str()
        .unwrap_or("")
        .to_string();
    let expires_in = token_resp["expires_in"].as_i64().unwrap_or(3600);
    let expires_at = chrono::Utc::now() + chrono::Duration::seconds(expires_in - 60);

    let profile: serde_json::Value = http
        .get("https://www.googleapis.com/oauth2/v2/userinfo")
        .bearer_auth(&access_token)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let email_address = profile["email"].as_str().unwrap_or("unknown").to_string();

    let conn = state
        .subscriptions
        .connect_gmail(
            user_id,
            ConnectGmailParams {
                email_address,
                access_token,
                refresh_token,
                expires_at,
            },
        )
        .await?;
    Ok((StatusCode::CREATED, Json(to_response(conn))))
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
    if state
        .subscriptions
        .connections
        .find_by_id(id, user_id)
        .await?
        .is_none()
    {
        return Err(SubscriptionError::ConnectionNotFound.into());
    }
    let new_ids = state.subscriptions.sync_connection(id).await?;
    if !new_ids.is_empty() {
        state.matcher.run_for_user(user_id).await?;
    }
    Ok(StatusCode::ACCEPTED)
}

pub async fn delete(
    State(state): State<AppState>,
    Extension(AuthUser(user_id)): Extension<AuthUser>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, AppError> {
    state.subscriptions.delete_connection(id, user_id).await?;
    Ok(StatusCode::NO_CONTENT)
}
