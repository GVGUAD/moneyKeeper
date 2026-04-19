use axum::{
    Extension, Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
};
use uuid::Uuid;

use crate::api::{
    dto::{
        ConnectMonobankRequest, MonoAccountResponse, MonobankConnectionResponse,
        MonobankWebhookPayload,
    },
    error::AppError,
    middleware::AuthUser,
    state::AppState,
};
use crate::domain::error::DomainError;
use crate::domain::monobank::MonobankConnection;

fn to_response(conn: MonobankConnection) -> MonobankConnectionResponse {
    MonobankConnectionResponse {
        id: conn.id,
        account_id: conn.account_id,
        monobank_account_id: conn.monobank_account_id,
        sync_status: conn.sync_status.as_str().to_string(),
        last_synced_at: conn.last_synced_at,
        created_at: conn.created_at,
    }
}

/// GET /monobank/client-info  (requires X-Token header)
pub async fn get_client_info(
    State(state): State<AppState>,
    Extension(AuthUser(_user_id)): Extension<AuthUser>,
    headers: HeaderMap,
) -> Result<Json<Vec<MonoAccountResponse>>, AppError> {
    let token = headers
        .get("x-token")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| DomainError::InvalidInput("missing X-Token header".to_string()))?;
    let accounts = state.monobank.get_monobank_accounts(token).await?;
    Ok(Json(
        accounts
            .into_iter()
            .map(|a| MonoAccountResponse {
                id: a.id,
                currency_code: a.currency_code,
                balance: a.balance,
                account_type: a.account_type,
                iban: a.iban,
            })
            .collect(),
    ))
}

/// POST /monobank/connect
pub async fn connect(
    State(state): State<AppState>,
    Extension(AuthUser(user_id)): Extension<AuthUser>,
    Json(req): Json<ConnectMonobankRequest>,
) -> Result<(StatusCode, Json<MonobankConnectionResponse>), AppError> {
    let (account, _) = state.accounts.get(req.account_id, user_id).await?;
    let account_created_at = account.created_at;

    let conn = state
        .monobank
        .connect(
            req.account_id,
            user_id,
            req.token,
            req.monobank_account_id,
            account_created_at,
        )
        .await?;

    Ok((StatusCode::CREATED, Json(to_response(conn))))
}

/// GET /monobank/connections
pub async fn list_connections(
    State(state): State<AppState>,
    Extension(AuthUser(user_id)): Extension<AuthUser>,
) -> Result<Json<Vec<MonobankConnectionResponse>>, AppError> {
    let conns = state.monobank.list_connections(user_id).await?;
    Ok(Json(conns.into_iter().map(to_response).collect()))
}

/// DELETE /monobank/connections/:id
pub async fn delete_connection(
    State(state): State<AppState>,
    Extension(AuthUser(user_id)): Extension<AuthUser>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, AppError> {
    state.monobank.delete_connection(id, user_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// POST /monobank/webhook  (public — no auth)
pub async fn webhook(
    State(state): State<AppState>,
    Json(payload): Json<MonobankWebhookPayload>,
) -> Result<StatusCode, AppError> {
    if payload.event_type != "StatementItem" {
        return Ok(StatusCode::OK);
    }
    if let Some(data) = payload.data {
        state
            .monobank
            .handle_webhook(&data.account, &data.statement_item)
            .await?;
    }
    Ok(StatusCode::OK)
}
