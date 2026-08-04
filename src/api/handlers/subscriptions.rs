use axum::{
    Extension, Json,
    extract::{Path, Query, State},
    http::StatusCode,
};
use uuid::Uuid;

use crate::api::{
    dto::{
        ForecastResponse, LinkChargeRequest, SubscriptionChargeResponse, SubscriptionListQuery,
        SubscriptionResponse, UpdateSubscriptionRequest,
    },
    error::AppError,
    middleware::AuthUser,
    state::AppState,
};
use crate::domain::subscription::{BillingPeriod, Subscription, SubscriptionStatus};
use crate::domain::subscription_charge::SubscriptionCharge;
use crate::domain::subscription_error::SubscriptionError;

fn to_resp(s: Subscription) -> SubscriptionResponse {
    SubscriptionResponse {
        id: s.id,
        provider: s.provider.as_str().to_string(),
        product_name: s.product_name,
        amount: s.amount,
        currency: s.currency,
        billing_period: s.billing_period.as_str().to_string(),
        status: s.status.as_str().to_string(),
        started_at: s.started_at,
        last_charged_at: s.last_charged_at,
        next_expected_at: s.next_expected_at,
        category_id: s.category_id,
        created_at: s.created_at,
    }
}

fn charge_resp(c: SubscriptionCharge) -> SubscriptionChargeResponse {
    SubscriptionChargeResponse {
        id: c.id,
        subscription_id: c.subscription_id,
        amount: c.amount,
        currency: c.currency,
        charged_at: c.charged_at,
        kind: c.kind.as_str().to_string(),
        transaction_id: c.transaction_id,
        match_status: c.match_status.as_str().to_string(),
    }
}

pub async fn list(
    State(state): State<AppState>,
    Extension(AuthUser(user_id)): Extension<AuthUser>,
    Query(q): Query<SubscriptionListQuery>,
) -> Result<Json<Vec<SubscriptionResponse>>, AppError> {
    let status = match q.status.as_deref() {
        Some("active") => Some(SubscriptionStatus::Active),
        Some("inactive") => Some(SubscriptionStatus::Inactive),
        Some("all") | None => None,
        Some(other) => return Err(anyhow::anyhow!("unknown status filter: {other}").into()),
    };
    let items = state.subscriptions.list(user_id, status).await?;
    Ok(Json(items.into_iter().map(to_resp).collect()))
}

pub async fn get(
    State(state): State<AppState>,
    Extension(AuthUser(user_id)): Extension<AuthUser>,
    Path(id): Path<Uuid>,
) -> Result<Json<SubscriptionResponse>, AppError> {
    let s = state
        .subscriptions
        .subscriptions
        .find_by_id(id, user_id)
        .await?
        .ok_or(SubscriptionError::SubscriptionNotFound)?;
    Ok(Json(to_resp(s)))
}

pub async fn patch(
    State(state): State<AppState>,
    Extension(AuthUser(user_id)): Extension<AuthUser>,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdateSubscriptionRequest>,
) -> Result<Json<SubscriptionResponse>, AppError> {
    let billing_period = req
        .billing_period
        .as_deref()
        .map(BillingPeriod::from_str)
        .transpose()
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let status = req
        .status
        .as_deref()
        .map(SubscriptionStatus::from_str)
        .transpose()
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    state
        .subscriptions
        .subscriptions
        .update_editable_fields(
            id,
            user_id,
            req.product_name,
            req.category_id,
            billing_period,
            status,
        )
        .await?;
    let s = state
        .subscriptions
        .subscriptions
        .find_by_id(id, user_id)
        .await?
        .ok_or(SubscriptionError::SubscriptionNotFound)?;
    Ok(Json(to_resp(s)))
}

pub async fn delete(
    State(state): State<AppState>,
    Extension(AuthUser(user_id)): Extension<AuthUser>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, AppError> {
    state
        .subscriptions
        .subscriptions
        .delete(id, user_id)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn list_charges(
    State(state): State<AppState>,
    Extension(AuthUser(user_id)): Extension<AuthUser>,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<SubscriptionChargeResponse>>, AppError> {
    let items = state
        .subscriptions
        .charges
        .list_for_subscription(id, user_id)
        .await?;
    Ok(Json(items.into_iter().map(charge_resp).collect()))
}

pub async fn forecast(
    State(state): State<AppState>,
    Extension(AuthUser(user_id)): Extension<AuthUser>,
) -> Result<Json<ForecastResponse>, AppError> {
    let settings = state.user_settings.get_or_default(user_id).await?;
    let f = state
        .subscriptions
        .forecast_next_30d(user_id, &settings.base_currency, &*state.fx)
        .await?;
    Ok(Json(ForecastResponse {
        base_currency: f.base_currency,
        base_total: f.base_total,
        by_currency: f.by_currency,
    }))
}

pub async fn link_charge(
    State(state): State<AppState>,
    Extension(AuthUser(user_id)): Extension<AuthUser>,
    Path(id): Path<Uuid>,
    Json(req): Json<LinkChargeRequest>,
) -> Result<StatusCode, AppError> {
    let charge = state
        .subscriptions
        .charges
        .find_by_id(id, user_id)
        .await?
        .ok_or(SubscriptionError::ChargeNotFound)?;
    // Verify the transaction exists and belongs to this user
    state.transactions.get(req.transaction_id, user_id).await?;
    state
        .subscriptions
        .charges
        .update_match(
            charge.id,
            Some(req.transaction_id),
            crate::domain::subscription_charge::ChargeMatchStatus::Matched,
        )
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn unlink_charge(
    State(state): State<AppState>,
    Extension(AuthUser(user_id)): Extension<AuthUser>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, AppError> {
    let charge = state
        .subscriptions
        .charges
        .find_by_id(id, user_id)
        .await?
        .ok_or(SubscriptionError::ChargeNotFound)?;
    state
        .subscriptions
        .charges
        .update_match(
            charge.id,
            None,
            crate::domain::subscription_charge::ChargeMatchStatus::Pending,
        )
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
