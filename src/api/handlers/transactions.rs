use axum::Json;
use axum::extract::{Extension, Path, Query, State};
use axum::http::StatusCode;
use chrono::{DateTime, Utc};
use std::collections::HashMap;
use uuid::Uuid;

use crate::api::dto::{
    CreateTransactionRequest, PaginationInfo, TransactionDetailsDto, TransactionListResponse,
    TransactionResponse, TxListQuery,
};
use crate::api::error::AppError;
use crate::api::middleware::AuthUser;
use crate::api::state::AppState;
use crate::domain::error::DomainError;
use crate::domain::subscription_charge::TransactionSubscriptionLink;
use crate::domain::transaction::{
    TradeDetails, Transaction, TransactionDetails, TransactionKind, TransactionListParams,
};

type DateRange = (Option<DateTime<Utc>>, Option<DateTime<Utc>>);

/// Parse optional `from`/`to` Unix-seconds query parameters into a validated date range.
/// Returns `(from, to)` after rejecting out-of-range values and inverted ranges.
fn parse_date_range(from: Option<i64>, to: Option<i64>) -> Result<DateRange, DomainError> {
    let parse = |s: i64, field: &str| {
        DateTime::<Utc>::from_timestamp(s, 0)
            .ok_or_else(|| DomainError::InvalidInput(format!("{field} is out of range")))
    };
    let from_dt = from.map(|s| parse(s, "from")).transpose()?;
    let to_dt = to.map(|s| parse(s, "to")).transpose()?;
    if let (Some(f), Some(t)) = (from_dt, to_dt)
        && f > t
    {
        return Err(DomainError::InvalidInput("from must be <= to".to_string()));
    }
    Ok((from_dt, to_dt))
}

fn dto_to_tx_details(dto: Option<TransactionDetailsDto>, tx_id: Uuid) -> TransactionDetails {
    match dto {
        Some(TransactionDetailsDto::Trade {
            ticker,
            quantity,
            price_per_unit,
            fee,
        }) => TransactionDetails::Trade(TradeDetails {
            transaction_id: tx_id,
            ticker,
            quantity,
            price_per_unit,
            fee,
        }),
        _ => TransactionDetails::None,
    }
}

fn tx_details_to_dto(details: &TransactionDetails) -> Option<TransactionDetailsDto> {
    match details {
        TransactionDetails::Trade(t) => Some(TransactionDetailsDto::Trade {
            ticker: t.ticker.clone(),
            quantity: t.quantity,
            price_per_unit: t.price_per_unit,
            fee: t.fee,
        }),
        TransactionDetails::Transfer(link) => Some(TransactionDetailsDto::Transfer {
            to_account_id: link.to_transaction_id,
        }),
        TransactionDetails::None => None,
    }
}

fn to_response(
    tx: Transaction,
    details: &TransactionDetails,
    link: Option<TransactionSubscriptionLink>,
) -> TransactionResponse {
    TransactionResponse {
        id: tx.id,
        account_id: tx.account_id,
        amount: tx.amount,
        currency: tx.currency,
        kind: tx.kind.as_str().to_string(),
        category_id: tx.category_id,
        note: tx.note,
        transacted_at: tx.transacted_at,
        created_at: tx.created_at,
        details: tx_details_to_dto(details),
        subscription_id: link.map(|value| value.subscription_id),
        subscription_charge_id: link.map(|value| value.charge_id),
    }
}

async fn links_by_transaction(
    state: &AppState,
    user_id: Uuid,
    ids: &[Uuid],
) -> Result<HashMap<Uuid, TransactionSubscriptionLink>, AppError> {
    Ok(state
        .subscriptions
        .find_transaction_links(user_id, ids)
        .await?
        .into_iter()
        .map(|link| (link.transaction_id, link))
        .collect())
}

pub async fn create_transaction(
    State(state): State<AppState>,
    Extension(AuthUser(user_id)): Extension<AuthUser>,
    Path(account_id): Path<Uuid>,
    Json(req): Json<CreateTransactionRequest>,
) -> Result<(StatusCode, Json<TransactionResponse>), AppError> {
    state.accounts.get(account_id, user_id).await?;
    let kind = TransactionKind::from_str(&req.kind)
        .map_err(|_| DomainError::InvalidInput(format!("unknown kind: {}", req.kind)))?;
    let details = dto_to_tx_details(req.details, Uuid::nil());
    let tx = state
        .transactions
        .create(
            account_id,
            user_id,
            req.amount,
            req.currency,
            kind,
            req.category_id,
            req.note,
            req.transacted_at,
            details.clone(),
        )
        .await?;
    Ok((StatusCode::CREATED, Json(to_response(tx, &details, None))))
}

pub async fn list_transactions(
    State(state): State<AppState>,
    Extension(AuthUser(user_id)): Extension<AuthUser>,
    Path(account_id): Path<Uuid>,
    Query(q): Query<TxListQuery>,
) -> Result<Json<TransactionListResponse>, AppError> {
    let kind = q
        .kind
        .map(|k| TransactionKind::from_str(&k))
        .transpose()
        .map_err(|_| DomainError::InvalidInput("unknown kind".to_string()))?;
    let (from, to) = parse_date_range(q.from, q.to)?;
    let params = TransactionListParams {
        account_id: Some(account_id),
        user_id,
        kind,
        category_id: q.category_id,
        from,
        to,
        limit: q.limit,
        offset: q.offset,
    };
    let txs = state.transactions.list(params.clone()).await?;
    let total = state.transactions.count(params).await?;
    let transaction_ids = txs.iter().map(|(tx, _)| tx.id).collect::<Vec<_>>();
    let links = links_by_transaction(&state, user_id, &transaction_ids).await?;
    Ok(Json(TransactionListResponse {
        items: txs
            .into_iter()
            .map(|(tx, details)| {
                let link = links.get(&tx.id).copied();
                to_response(tx, &details, link)
            })
            .collect(),
        pagination: PaginationInfo {
            total,
            limit: q.limit,
            offset: q.offset,
        },
    }))
}

pub async fn get_transaction(
    State(state): State<AppState>,
    Extension(AuthUser(user_id)): Extension<AuthUser>,
    Path(id): Path<Uuid>,
) -> Result<Json<TransactionResponse>, AppError> {
    let (tx, d) = state.transactions.get(id, user_id).await?;
    let link = links_by_transaction(&state, user_id, &[id])
        .await?
        .remove(&id);
    Ok(Json(to_response(tx, &d, link)))
}

pub async fn delete_transaction(
    State(state): State<AppState>,
    Extension(AuthUser(user_id)): Extension<AuthUser>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, AppError> {
    state.transactions.delete(id, user_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn list_all_transactions(
    State(state): State<AppState>,
    Extension(AuthUser(user_id)): Extension<AuthUser>,
    Query(q): Query<TxListQuery>,
) -> Result<Json<TransactionListResponse>, AppError> {
    let (from, to) = parse_date_range(q.from, q.to)?;
    let params = TransactionListParams {
        account_id: None,
        user_id,
        kind: None,
        category_id: None,
        from,
        to,
        limit: q.limit,
        offset: q.offset,
    };
    let txs = state.transactions.list(params.clone()).await?;
    let total = state.transactions.count(params).await?;
    let transaction_ids = txs.iter().map(|(tx, _)| tx.id).collect::<Vec<_>>();
    let links = links_by_transaction(&state, user_id, &transaction_ids).await?;
    Ok(Json(TransactionListResponse {
        items: txs
            .into_iter()
            .map(|(tx, details)| {
                let link = links.get(&tx.id).copied();
                to_response(tx, &details, link)
            })
            .collect(),
        pagination: PaginationInfo {
            total,
            limit: q.limit,
            offset: q.offset,
        },
    }))
}
