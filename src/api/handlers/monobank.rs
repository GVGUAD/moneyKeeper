use axum::{
    Extension, Json,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
};
use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::api::{
    dto::{
        ConnectMonobankRequest, MonoAccountResponse, MonobankConnectionResponse,
        MonobankWebhookPayload, ResyncJobResponse, ResyncQuery,
    },
    error::AppError,
    middleware::AuthUser,
    state::AppState,
};
use crate::domain::bank_connection::BankConnection;
use crate::domain::error::DomainError;
use crate::domain::secret::SecretString;

fn to_response(conn: BankConnection) -> MonobankConnectionResponse {
    MonobankConnectionResponse {
        id: conn.id,
        account_id: conn.account_id,
        provider: conn.provider.as_str().to_string(),
        external_account_id: conn.external_account_id,
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
    let token = SecretString::new(token);
    let accounts = state.monobank.get_monobank_accounts(&token).await?;
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
            req.external_account_id,
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

/// POST /monobank/connections/:id/resync?from=...&to=...
pub async fn resync_connection(
    State(state): State<AppState>,
    Extension(AuthUser(user_id)): Extension<AuthUser>,
    Path(id): Path<Uuid>,
    Query(q): Query<ResyncQuery>,
) -> Result<(StatusCode, Json<ResyncJobResponse>), AppError> {
    if q.from < 0 || q.to < 0 {
        return Err(DomainError::InvalidInput("`from` and `to` must be >= 0".into()).into());
    }
    if q.to < q.from {
        return Err(DomainError::InvalidInput("`to` must be >= `from`".into()).into());
    }
    let from = DateTime::<Utc>::from_timestamp(q.from, 0)
        .ok_or_else(|| DomainError::InvalidInput("invalid `from`".into()))?;
    let to = DateTime::<Utc>::from_timestamp(q.to, 0)
        .ok_or_else(|| DomainError::InvalidInput("invalid `to`".into()))?;

    let conn = state.monobank.resync_window(user_id, id, from, to).await?;

    Ok((
        StatusCode::ACCEPTED,
        Json(ResyncJobResponse {
            connection_id: conn.id,
            sync_status: conn.sync_status.as_str().to_string(),
            from: q.from,
            to: q.to,
            enqueued_at: Utc::now(),
        }),
    ))
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
