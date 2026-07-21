use axum::{
    Extension, Json,
    extract::{Path, Query, State, rejection::JsonRejection},
    http::StatusCode,
};
use uuid::Uuid;

use crate::api::{
    dto::{
        ForecastFxQuoteResponse, ForecastNormalizedCurrencyResponse, ForecastResponse,
        LinkChargeRequest, SubscriptionChargeResponse, SubscriptionListQuery,
        SubscriptionOverridesResponse, SubscriptionResponse, UpdateSubscriptionRequest,
    },
    error::AppError,
    middleware::AuthUser,
    state::AppState,
};
use crate::domain::error::DomainError;
use crate::domain::subscription::{BillingPeriod, Subscription, SubscriptionStatus};
use crate::domain::subscription_charge::{ChargeLinkOutcome, SubscriptionCharge};
use crate::domain::subscription_error::SubscriptionError;

fn to_resp(
    s: Subscription,
    charges: Option<Vec<SubscriptionChargeResponse>>,
) -> SubscriptionResponse {
    let next_expected_at = if s.overrides.billing_period.is_some() {
        Some(
            s.billing_period
                .next_after(s.last_charged_at.unwrap_or(s.started_at)),
        )
    } else {
        s.next_expected_at
    };
    let overrides = SubscriptionOverridesResponse {
        product_name: s.overrides.product_name,
        billing_period: s
            .overrides
            .billing_period
            .map(|value| value.as_str().to_string()),
        status: s.overrides.status.map(|value| value.as_str().to_string()),
    };
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
        next_expected_at,
        category_id: s.category_id,
        overrides,
        charges,
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
        Some(other) => {
            return Err(
                DomainError::InvalidInput(format!("unknown status filter: {other}")).into(),
            );
        }
    };
    let items = state.subscriptions.list(user_id, status).await?;
    Ok(Json(
        items
            .into_iter()
            .map(|subscription| to_resp(subscription, None))
            .collect(),
    ))
}

pub async fn get(
    State(state): State<AppState>,
    Extension(AuthUser(user_id)): Extension<AuthUser>,
    Path(id): Path<Uuid>,
) -> Result<Json<SubscriptionResponse>, AppError> {
    let s = state.subscriptions.get(user_id, id).await?;
    let charges = state
        .subscriptions
        .list_charges(user_id, id, Some(20))
        .await?
        .into_iter()
        .map(charge_resp)
        .collect();
    Ok(Json(to_resp(s, Some(charges))))
}

pub async fn patch(
    State(state): State<AppState>,
    Extension(AuthUser(user_id)): Extension<AuthUser>,
    Path(id): Path<Uuid>,
    request: Result<Json<UpdateSubscriptionRequest>, JsonRejection>,
) -> Result<Json<SubscriptionResponse>, AppError> {
    let Json(req) = request
        .map_err(|_| DomainError::InvalidInput("invalid subscription update body".into()))?;
    let product_name = match req.product_name {
        Some(Some(value)) if value.trim().is_empty() => {
            return Err(DomainError::InvalidInput("product_name cannot be empty".into()).into());
        }
        Some(Some(value)) => Some(Some(value.trim().to_string())),
        Some(None) => Some(None),
        None => None,
    };
    let billing_period = match req.billing_period {
        Some(Some(value)) => {
            Some(Some(BillingPeriod::from_str(&value).map_err(|_| {
                DomainError::InvalidInput("invalid billing_period".into())
            })?))
        }
        Some(None) => Some(None),
        None => None,
    };
    let status = match req.status.as_ref() {
        Some(Some(value)) if value == "active" => Some(Some(SubscriptionStatus::Active)),
        Some(Some(value)) if value == "inactive" => Some(Some(SubscriptionStatus::Inactive)),
        Some(Some(value)) if value == "auto" => Some(None),
        Some(Some(_)) | Some(None) => {
            return Err(DomainError::InvalidInput("invalid status".into()).into());
        }
        None => None,
    };
    let s = state
        .subscriptions
        .update_overrides(
            user_id,
            id,
            product_name,
            req.category_id,
            billing_period,
            status,
        )
        .await?;
    Ok(Json(to_resp(s, None)))
}

pub async fn delete(
    State(state): State<AppState>,
    Extension(AuthUser(user_id)): Extension<AuthUser>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, AppError> {
    state.subscriptions.delete_subscription(user_id, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn list_charges(
    State(state): State<AppState>,
    Extension(AuthUser(user_id)): Extension<AuthUser>,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<SubscriptionChargeResponse>>, AppError> {
    let items = state.subscriptions.list_charges(user_id, id, None).await?;
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
        window_start: f.window_start,
        window_end: f.window_end,
        base_currency: f.base_currency,
        base_total: f.base_total,
        by_currency: f.by_currency,
        monthly_equivalent_total: f.monthly_equivalent_total,
        yearly_equivalent_total: f.yearly_equivalent_total,
        normalized_by_currency: f
            .normalized_by_currency
            .into_iter()
            .map(|(currency, totals)| {
                (
                    currency,
                    ForecastNormalizedCurrencyResponse {
                        monthly: totals.monthly,
                        yearly: totals.yearly,
                    },
                )
            })
            .collect(),
        fx_quotes: f
            .fx_quotes
            .into_iter()
            .map(|quote| ForecastFxQuoteResponse {
                from_currency: quote.from_currency,
                to_currency: quote.to_currency,
                rate: quote.rate,
                requested_date: quote.requested_date,
                rate_date: quote.rate_date,
            })
            .collect(),
        complete: f.complete,
        warnings: f.warnings,
    }))
}

pub async fn link_charge(
    State(state): State<AppState>,
    Extension(AuthUser(user_id)): Extension<AuthUser>,
    Path(id): Path<Uuid>,
    request: Result<Json<LinkChargeRequest>, JsonRejection>,
) -> Result<StatusCode, AppError> {
    let Json(req) =
        request.map_err(|_| DomainError::InvalidInput("invalid charge link body".into()))?;
    let transaction = state.transactions.get(req.transaction_id, user_id).await?;
    if transaction.0.kind != crate::domain::transaction::TransactionKind::Expense {
        return Err(DomainError::InvalidInput(
            "subscription charges can only link to expense transactions".into(),
        )
        .into());
    }
    let outcome = state
        .subscriptions
        .manual_link_charge(user_id, id, req.transaction_id)
        .await?;
    match outcome {
        ChargeLinkOutcome::Linked => {}
        ChargeLinkOutcome::ChargeNotFound => {
            return Err(SubscriptionError::ChargeNotFound.into());
        }
        ChargeLinkOutcome::TransactionNotFound => {
            return Err(
                DomainError::NotFound(format!("transaction {}", req.transaction_id)).into(),
            );
        }
        ChargeLinkOutcome::TransactionNotExpense => {
            return Err(DomainError::InvalidInput("transaction is not an expense".into()).into());
        }
        ChargeLinkOutcome::ChargeNotPending
        | ChargeLinkOutcome::ChargeAlreadyLinked
        | ChargeLinkOutcome::TransactionAlreadyLinked => {
            return Err(
                DomainError::Conflict("charge or transaction is already linked".into()).into(),
            );
        }
    }
    Ok(StatusCode::NO_CONTENT)
}

pub async fn unlink_charge(
    State(state): State<AppState>,
    Extension(AuthUser(user_id)): Extension<AuthUser>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, AppError> {
    state
        .subscriptions
        .manual_unlink_charge(user_id, id)
        .await?;
    // The rejected pair is excluded by the matcher, so retrying now can safely
    // select another candidate without immediately restoring the manual link.
    state.matcher.run_for_user(user_id).await?;
    Ok(StatusCode::NO_CONTENT)
}
