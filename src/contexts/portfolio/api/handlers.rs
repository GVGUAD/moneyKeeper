use super::dto::*;
use crate::api::{ApiError, ApiJson, AuthenticatedUser};
use crate::contexts::ledger::public::LedgerAccountId;
use crate::contexts::portfolio::public::*;
use crate::shared_kernel::{CurrencyCode, IdempotencyKey};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::HeaderMap,
};
use chrono::Utc;
use rust_decimal::Decimal;
use std::str::FromStr;
use uuid::Uuid;

pub(crate) async fn create_ovdp(
    State(f): State<PortfolioFacade>,
    AuthenticatedUser(user): AuthenticatedUser,
    headers: HeaderMap,
    ApiJson(b): ApiJson<OvdpBody>,
) -> Result<(axum::http::StatusCode, Json<PortfolioCommandResult>), ApiError> {
    let coupon_terms = match b.coupon_kind.as_str() {
        "fixed" => CouponTerms::Fixed {
            annual_rate: decimal(
                b.coupon_rate
                    .as_deref()
                    .ok_or_else(|| ApiError::bad_request("coupon_rate required"))?,
            )?,
        },
        "zero_coupon" => CouponTerms::ZeroCoupon,
        "unknown" => CouponTerms::Unknown,
        _ => return Err(ApiError::bad_request("invalid coupon_kind")),
    };
    let c = CreateManualOvdpInstrument {
        user_id: user,
        identifier: InstrumentIdentifier::new(
            if b.identifier_kind == "isin" {
                IdentifierKind::Isin
            } else {
                IdentifierKind::Manual
            },
            b.identifier,
        )
        .map_err(|_| ApiError::bad_request("invalid identifier"))?,
        display_name: b.display_name,
        currency: currency(&b.currency)?,
        face_value: decimal(&b.face_value)?,
        issue_date: b.issue_date,
        maturity_date: b.maturity_date,
        coupon_terms,
        idempotency_key: key(&headers)?,
        correlation_id: crate::api::request_correlation_id(),
        occurred_at: Utc::now(),
    };
    Ok((
        axum::http::StatusCode::CREATED,
        Json(f.create_manual_ovdp(c).await.map_err(map)?),
    ))
}
pub(crate) async fn open_account(
    State(f): State<PortfolioFacade>,
    AuthenticatedUser(user): AuthenticatedUser,
    headers: HeaderMap,
    ApiJson(b): ApiJson<AccountBody>,
) -> Result<(axum::http::StatusCode, Json<PortfolioCommandResult>), ApiError> {
    let c = OpenPortfolioAccount {
        user_id: user,
        name: b.name,
        idempotency_key: key(&headers)?,
        correlation_id: crate::api::request_correlation_id(),
        occurred_at: Utc::now(),
    };
    Ok((
        axum::http::StatusCode::CREATED,
        Json(f.open_account(c).await.map_err(map)?),
    ))
}
pub(crate) async fn rename_account(
    State(f): State<PortfolioFacade>,
    AuthenticatedUser(user): AuthenticatedUser,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    ApiJson(b): ApiJson<AccountBody>,
) -> Result<Json<PortfolioCommandResult>, ApiError> {
    let c = change(
        user,
        id,
        b.expected_version
            .ok_or_else(|| ApiError::bad_request("expected_version required"))?,
        Some(b.name),
        &headers,
    )?;
    Ok(Json(f.rename_account(c).await.map_err(map)?))
}
pub(crate) async fn archive_account(
    State(f): State<PortfolioFacade>,
    AuthenticatedUser(user): AuthenticatedUser,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    ApiJson(b): ApiJson<VersionBody>,
) -> Result<Json<PortfolioCommandResult>, ApiError> {
    Ok(Json(
        f.archive_account(change(user, id, b.expected_version, None, &headers)?)
            .await
            .map_err(map)?,
    ))
}
pub(crate) async fn restore_account(
    State(f): State<PortfolioFacade>,
    AuthenticatedUser(user): AuthenticatedUser,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    ApiJson(b): ApiJson<VersionBody>,
) -> Result<Json<PortfolioCommandResult>, ApiError> {
    Ok(Json(
        f.restore_account(change(user, id, b.expected_version, None, &headers)?)
            .await
            .map_err(map)?,
    ))
}
pub(crate) async fn accounts(
    State(f): State<PortfolioFacade>,
    AuthenticatedUser(user): AuthenticatedUser,
) -> Result<Json<Vec<PortfolioAccountView>>, ApiError> {
    Ok(Json(f.accounts(user).await.map_err(map)?))
}
pub(crate) async fn account(
    State(f): State<PortfolioFacade>,
    AuthenticatedUser(user): AuthenticatedUser,
    Path(id): Path<Uuid>,
) -> Result<Json<PortfolioAccountView>, ApiError> {
    f.account(user, PortfolioAccountId::new(id))
        .await
        .map_err(map)?
        .map(Json)
        .ok_or_else(|| ApiError::not_found("portfolio account not found"))
}
pub(crate) async fn instruments(
    State(f): State<PortfolioFacade>,
    AuthenticatedUser(user): AuthenticatedUser,
) -> Result<Json<Vec<InstrumentView>>, ApiError> {
    Ok(Json(f.instruments(user).await.map_err(map)?))
}
pub(crate) async fn instrument(
    State(f): State<PortfolioFacade>,
    AuthenticatedUser(user): AuthenticatedUser,
    Path(id): Path<Uuid>,
) -> Result<Json<InstrumentView>, ApiError> {
    f.instrument(user, InstrumentId::new(id))
        .await
        .map_err(map)?
        .map(Json)
        .ok_or_else(|| ApiError::not_found("instrument not found"))
}
pub(crate) async fn activity(
    State(f): State<PortfolioFacade>,
    AuthenticatedUser(user): AuthenticatedUser,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<PortfolioTransactionView>>, ApiError> {
    Ok(Json(
        f.activity(user, PortfolioAccountId::new(id))
            .await
            .map_err(map)?,
    ))
}
pub(crate) async fn positions(
    State(f): State<PortfolioFacade>,
    AuthenticatedUser(user): AuthenticatedUser,
    Query(q): Query<PositionParams>,
) -> Result<Json<Vec<PositionView>>, ApiError> {
    Ok(Json(
        f.positions(user, PortfolioAccountId::new(q.portfolio_account_id))
            .await
            .map_err(map)?,
    ))
}
pub(crate) async fn valuations(
    State(f): State<PortfolioFacade>,
    AuthenticatedUser(user): AuthenticatedUser,
    Query(q): Query<ValuationParams>,
) -> Result<Json<Vec<ValuationView>>, ApiError> {
    Ok(Json(
        f.valuations(
            user,
            PortfolioAccountId::new(q.portfolio_account_id),
            InstrumentId::new(q.instrument_id),
        )
        .await
        .map_err(map)?,
    ))
}
pub(crate) async fn record_valuation(
    State(f): State<PortfolioFacade>,
    AuthenticatedUser(user): AuthenticatedUser,
    headers: HeaderMap,
    ApiJson(b): ApiJson<ValuationBody>,
) -> Result<(axum::http::StatusCode, Json<PortfolioCommandResult>), ApiError> {
    let c = RecordValuationSnapshot {
        user_id: user,
        account_id: PortfolioAccountId::new(b.portfolio_account_id),
        instrument_id: InstrumentId::new(b.instrument_id),
        price_per_instrument: decimal(&b.price_per_instrument)?,
        accrued_interest_per_instrument: decimal(&b.accrued_interest_per_instrument)?,
        currency: currency(&b.currency)?,
        source: b.source,
        quoted_at: b.quoted_at,
        idempotency_key: key(&headers)?,
        correlation_id: crate::api::request_correlation_id(),
        recorded_at: Utc::now(),
    };
    Ok((
        axum::http::StatusCode::CREATED,
        Json(f.record_valuation(c).await.map_err(map)?),
    ))
}
pub(crate) async fn record_transaction(
    State(f): State<PortfolioFacade>,
    AuthenticatedUser(user): AuthenticatedUser,
    headers: HeaderMap,
    ApiJson(b): ApiJson<TransactionBody>,
) -> Result<(axum::http::StatusCode, Json<PortfolioCommandResult>), ApiError> {
    let quantity = || {
        b.quantity
            .as_deref()
            .ok_or_else(|| ApiError::bad_request("quantity required"))
            .and_then(decimal)
    };
    let date = b.effective_date.unwrap_or_else(|| Utc::now().date_naive());
    let at = b.effective_at.unwrap_or_else(Utc::now);
    let allocations = b
        .lot_allocations
        .map(|v| {
            v.into_iter()
                .map(|a| {
                    Ok(RequestedLotAllocation {
                        lot_id: LotId::new(a.lot_id),
                        quantity: decimal(&a.quantity)?,
                    })
                })
                .collect::<Result<Vec<_>, ApiError>>()
        })
        .transpose()?;
    let activity = match b.kind.as_str() {
        "opening_position" => PortfolioActivityCommand::OpeningPosition {
            quantity: quantity()?,
            acquisition_cost: b.acquisition_cost.as_deref().map(decimal).transpose()?,
            acquisition_date: date,
            reason: b.reason.unwrap_or_else(|| "Opening position".into()),
        },
        "buy" => PortfolioActivityCommand::Buy {
            quantity: quantity()?,
            total_acquisition_cost: decimal(
                b.acquisition_cost
                    .as_deref()
                    .ok_or_else(|| ApiError::bad_request("acquisition_cost required"))?,
            )?,
            fee: b.fee.as_deref().map(decimal).transpose()?,
            accrued_interest: b.accrued_interest.as_deref().map(decimal).transpose()?,
            trade_at: at,
        },
        "sell" => PortfolioActivityCommand::Sell {
            quantity: quantity()?,
            proceeds: decimal(
                b.proceeds
                    .as_deref()
                    .ok_or_else(|| ApiError::bad_request("proceeds required"))?,
            )?,
            fee: b.fee.as_deref().map(decimal).transpose()?,
            trade_at: at,
            lot_allocations: allocations,
        },
        "coupon" => PortfolioActivityCommand::Coupon {
            amount: decimal(
                b.amount
                    .as_deref()
                    .ok_or_else(|| ApiError::bad_request("amount required"))?,
            )?,
            ex_date: None,
            payment_date: date,
        },
        "redemption" => PortfolioActivityCommand::Redemption {
            quantity: quantity()?,
            proceeds: decimal(
                b.proceeds
                    .as_deref()
                    .ok_or_else(|| ApiError::bad_request("proceeds required"))?,
            )?,
            maturity_date: date,
            reference: b.reason.unwrap_or_else(|| "Maturity".into()),
            lot_allocations: allocations,
        },
        "position_correction" => PortfolioActivityCommand::PositionCorrection {
            quantity_delta: quantity()?,
            cost_delta: b.acquisition_cost.as_deref().map(decimal).transpose()?,
            reason: b
                .reason
                .ok_or_else(|| ApiError::bad_request("reason required"))?,
            effective_at: at,
        },
        _ => return Err(ApiError::bad_request("invalid transaction kind")),
    };
    let cash = match (b.cash_account_id, b.cash_amount) {
        (Some(id), Some(amount)) => Some(OptionalCashSettlement {
            cash_account_id: LedgerAccountId::new(id),
            amount: decimal(&amount)?,
        }),
        (None, None) => None,
        _ => {
            return Err(ApiError::bad_request(
                "cash account and amount must be provided together",
            ));
        }
    };
    let c = RecordPortfolioTransaction {
        user_id: user,
        account_id: PortfolioAccountId::new(b.portfolio_account_id),
        instrument_id: InstrumentId::new(b.instrument_id),
        expected_account_version: b.expected_account_version,
        expected_position_version: b.expected_position_version,
        activity,
        cash_settlement: cash,
        actor_id: PortfolioActorId::new(user.into_uuid()),
        idempotency_key: key(&headers)?,
        correlation_id: crate::api::request_correlation_id(),
        recorded_at: Utc::now(),
    };
    Ok((
        axum::http::StatusCode::CREATED,
        Json(f.record_transaction(c).await.map_err(map)?),
    ))
}
pub(crate) async fn reverse_transaction(
    State(f): State<PortfolioFacade>,
    AuthenticatedUser(user): AuthenticatedUser,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    ApiJson(b): ApiJson<ReversalBody>,
) -> Result<(axum::http::StatusCode, Json<PortfolioCommandResult>), ApiError> {
    let c = ReversePortfolioTransaction {
        user_id: user,
        transaction_id: PortfolioTransactionId::new(id),
        expected_account_version: b.expected_account_version,
        expected_position_version: b.expected_position_version,
        reason: b.reason,
        actor_id: PortfolioActorId::new(user.into_uuid()),
        idempotency_key: key(&headers)?,
        correlation_id: crate::api::request_correlation_id(),
        recorded_at: Utc::now(),
    };
    Ok((
        axum::http::StatusCode::CREATED,
        Json(f.reverse_transaction(c).await.map_err(map)?),
    ))
}
fn change(
    user: crate::shared_kernel::UserId,
    id: Uuid,
    expected: u64,
    name: Option<String>,
    h: &HeaderMap,
) -> Result<ChangePortfolioAccount, ApiError> {
    Ok(ChangePortfolioAccount {
        user_id: user,
        account_id: PortfolioAccountId::new(id),
        expected_version: expected,
        name,
        idempotency_key: key(h)?,
        correlation_id: crate::api::request_correlation_id(),
        occurred_at: Utc::now(),
    })
}
fn key(h: &HeaderMap) -> Result<IdempotencyKey, ApiError> {
    h.get("Idempotency-Key")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| ApiError::bad_request("Idempotency-Key required"))
        .and_then(|v| {
            IdempotencyKey::new(v).map_err(|_| ApiError::bad_request("invalid Idempotency-Key"))
        })
}
fn decimal(v: &str) -> Result<Decimal, ApiError> {
    Decimal::from_str(v).map_err(|_| ApiError::bad_request("invalid decimal string"))
}
fn currency(v: &str) -> Result<CurrencyCode, ApiError> {
    CurrencyCode::new(v).map_err(|_| ApiError::bad_request("invalid currency"))
}
fn map(e: PortfolioFacadeError) -> ApiError {
    if e.is_not_found() {
        ApiError::not_found("portfolio fact not found")
    } else if e.is_conflict() {
        ApiError::conflict("portfolio command conflict")
    } else {
        ApiError::bad_request("invalid portfolio command")
    }
}
