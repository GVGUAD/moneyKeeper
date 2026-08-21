//! Loan task handlers.

use std::str::FromStr;

use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
};
use chrono::Utc;
use rust_decimal::Decimal;

use super::dto::*;
use super::routes::LoansApiState;
use crate::api::v2::{AuthenticatedUser, V2ApiError, V2Json};
use crate::contexts::loans::public::{
    LoanAgreementId, LoanDirection, LoanMovementId, MovementAmounts, MovementKind, OpenLoan,
    RecordLoanMovement, ReviseLoanTerms,
};
use crate::contexts::reference_data::public::CurrencyCatalog;
use crate::shared_kernel::{CorrelationId, CurrencyCode, IdempotencyKey};

pub(crate) async fn list(
    State(state): State<LoansApiState>,
    AuthenticatedUser(user): AuthenticatedUser,
) -> Result<Json<serde_json::Value>, V2ApiError> {
    let loans = state.loans.list(user).await.map_err(map_error)?;
    Ok(Json(serde_json::json!({"loans":loans})))
}
pub(crate) async fn get(
    State(state): State<LoansApiState>,
    AuthenticatedUser(user): AuthenticatedUser,
    Path(id): Path<uuid::Uuid>,
) -> Result<Json<serde_json::Value>, V2ApiError> {
    let loan = state
        .loans
        .get(user, LoanAgreementId::new(id))
        .await
        .map_err(map_error)?
        .ok_or_else(|| V2ApiError::not_found("loan was not found"))?;
    Ok(Json(
        serde_json::to_value(loan).map_err(|_| V2ApiError::internal())?,
    ))
}
pub(crate) async fn terms(
    State(state): State<LoansApiState>,
    AuthenticatedUser(user): AuthenticatedUser,
    Path(id): Path<uuid::Uuid>,
) -> Result<Json<serde_json::Value>, V2ApiError> {
    let rows = state
        .loans
        .term_revisions(user, LoanAgreementId::new(id))
        .await
        .map_err(map_error)?;
    Ok(Json(serde_json::json!({"term_revisions":rows})))
}
pub(crate) async fn movements(
    State(state): State<LoansApiState>,
    AuthenticatedUser(user): AuthenticatedUser,
    Path(id): Path<uuid::Uuid>,
) -> Result<Json<serde_json::Value>, V2ApiError> {
    let rows = state
        .loans
        .movements(user, LoanAgreementId::new(id))
        .await
        .map_err(map_error)?;
    Ok(Json(serde_json::json!({"movements":rows})))
}
pub(crate) async fn movement(
    State(state): State<LoansApiState>,
    AuthenticatedUser(user): AuthenticatedUser,
    Path((id, movement)): Path<(uuid::Uuid, uuid::Uuid)>,
) -> Result<Json<serde_json::Value>, V2ApiError> {
    let row = state
        .loans
        .movement(
            user,
            LoanAgreementId::new(id),
            LoanMovementId::new(movement),
        )
        .await
        .map_err(map_error)?
        .ok_or_else(|| V2ApiError::not_found("loan movement was not found"))?;
    Ok(Json(
        serde_json::to_value(row).map_err(|_| V2ApiError::internal())?,
    ))
}

pub(crate) async fn open(
    State(state): State<LoansApiState>,
    AuthenticatedUser(user): AuthenticatedUser,
    headers: HeaderMap,
    V2Json(body): V2Json<OpenLoanBody>,
) -> Result<(StatusCode, Json<serde_json::Value>), V2ApiError> {
    let key = idempotency(&headers)?;
    let (currency, minor_unit) = currency(&state, &body.currency).await?;
    let direction = match body.direction.as_str() {
        "borrowed" => LoanDirection::Borrowed,
        "lent" => LoanDirection::Lent,
        _ => return Err(V2ApiError::bad_request("invalid loan direction")),
    };
    let result = state
        .loans
        .open(OpenLoan {
            user_id: user,
            direction,
            counterparty: body.counterparty,
            contractual_principal: money_decimal(&body.contractual_principal, minor_unit)?,
            currency,
            start_date: body.start_date,
            due_date: body.due_date,
            annual_rate: body.annual_rate.as_deref().map(rate_decimal).transpose()?,
            idempotency_key: key,
            correlation_id: CorrelationId::generate(),
            occurred_at: Utc::now(),
        })
        .await
        .map_err(map_error)?;
    Ok((
        StatusCode::ACCEPTED,
        Json(serde_json::to_value(result).map_err(|_| V2ApiError::internal())?),
    ))
}

pub(crate) async fn revise(
    State(state): State<LoansApiState>,
    AuthenticatedUser(user): AuthenticatedUser,
    Path(id): Path<uuid::Uuid>,
    headers: HeaderMap,
    V2Json(body): V2Json<ReviseTermsBody>,
) -> Result<Json<serde_json::Value>, V2ApiError> {
    let key = idempotency(&headers)?;
    let (currency, minor_unit) = currency(&state, &body.currency).await?;
    let result = state
        .loans
        .revise_terms(ReviseLoanTerms {
            user_id: user,
            agreement_id: LoanAgreementId::new(id),
            counterparty: body.counterparty,
            contractual_principal: money_decimal(&body.contractual_principal, minor_unit)?,
            currency,
            start_date: body.start_date,
            due_date: body.due_date,
            annual_rate: body.annual_rate.as_deref().map(rate_decimal).transpose()?,
            reason: body.reason,
            expected_version: body.expected_version,
            idempotency_key: key,
            correlation_id: CorrelationId::generate(),
            occurred_at: Utc::now(),
        })
        .await
        .map_err(map_error)?;
    Ok(Json(
        serde_json::to_value(result).map_err(|_| V2ApiError::internal())?,
    ))
}

pub(crate) async fn disburse(
    s: State<LoansApiState>,
    u: AuthenticatedUser,
    p: Path<uuid::Uuid>,
    h: HeaderMap,
    b: V2Json<MovementBody>,
) -> Result<(StatusCode, Json<serde_json::Value>), V2ApiError> {
    record(s, u, p, h, b, MovementKind::Disbursement, None).await
}
pub(crate) async fn repay(
    s: State<LoansApiState>,
    u: AuthenticatedUser,
    p: Path<uuid::Uuid>,
    h: HeaderMap,
    b: V2Json<MovementBody>,
) -> Result<(StatusCode, Json<serde_json::Value>), V2ApiError> {
    record(s, u, p, h, b, MovementKind::Repayment, None).await
}
pub(crate) async fn accrue(
    s: State<LoansApiState>,
    u: AuthenticatedUser,
    p: Path<uuid::Uuid>,
    h: HeaderMap,
    b: V2Json<MovementBody>,
) -> Result<(StatusCode, Json<serde_json::Value>), V2ApiError> {
    record(s, u, p, h, b, MovementKind::Accrual, None).await
}
pub(crate) async fn write_off(
    s: State<LoansApiState>,
    u: AuthenticatedUser,
    p: Path<uuid::Uuid>,
    h: HeaderMap,
    b: V2Json<MovementBody>,
) -> Result<(StatusCode, Json<serde_json::Value>), V2ApiError> {
    record(s, u, p, h, b, MovementKind::WriteOff, None).await
}

async fn record(
    State(state): State<LoansApiState>,
    AuthenticatedUser(user): AuthenticatedUser,
    Path(id): Path<uuid::Uuid>,
    headers: HeaderMap,
    V2Json(body): V2Json<MovementBody>,
    kind: MovementKind,
    replaces: Option<LoanMovementId>,
) -> Result<(StatusCode, Json<serde_json::Value>), V2ApiError> {
    let key = idempotency(&headers)?;
    let (currency, minor_unit) = currency(&state, &body.currency).await?;
    let amounts = MovementAmounts {
        principal: money_decimal(&body.principal, minor_unit)?,
        accrued_interest: money_decimal(&body.accrued_interest, minor_unit)?,
        accrued_fee: money_decimal(&body.accrued_fee, minor_unit)?,
        current_interest: money_decimal(&body.current_interest, minor_unit)?,
        current_fee: money_decimal(&body.current_fee, minor_unit)?,
    };
    let result = state
        .loans
        .record_movement(RecordLoanMovement {
            user_id: user,
            agreement_id: LoanAgreementId::new(id),
            kind,
            currency,
            amounts,
            cash_account_id: body
                .cash_account_id
                .map(crate::contexts::ledger::public::LedgerAccountId::new),
            reason: body.reason,
            replaces,
            expected_version: body.expected_version,
            idempotency_key: key,
            correlation_id: CorrelationId::generate(),
            occurred_at: Utc::now(),
        })
        .await
        .map_err(map_error)?;
    Ok((
        StatusCode::ACCEPTED,
        Json(serde_json::to_value(result).map_err(|_| V2ApiError::internal())?),
    ))
}

pub(crate) async fn close(
    State(state): State<LoansApiState>,
    AuthenticatedUser(user): AuthenticatedUser,
    Path(id): Path<uuid::Uuid>,
    headers: HeaderMap,
    V2Json(body): V2Json<ClosureBody>,
) -> Result<Json<serde_json::Value>, V2ApiError> {
    let result = state
        .loans
        .close(
            user,
            LoanAgreementId::new(id),
            body.expected_version,
            idempotency(&headers)?,
            CorrelationId::generate(),
            Utc::now(),
        )
        .await
        .map_err(map_error)?;
    Ok(Json(
        serde_json::to_value(result).map_err(|_| V2ApiError::internal())?,
    ))
}

pub(crate) async fn reverse(
    State(state): State<LoansApiState>,
    AuthenticatedUser(user): AuthenticatedUser,
    Path((id, movement)): Path<(uuid::Uuid, uuid::Uuid)>,
    headers: HeaderMap,
    V2Json(body): V2Json<ReversalBody>,
) -> Result<(StatusCode, Json<serde_json::Value>), V2ApiError> {
    let key = idempotency(&headers)?;
    if body.reason.is_empty() || body.reason.trim() != body.reason {
        return Err(V2ApiError::bad_request("reason is required"));
    }
    let result = state
        .loans
        .request_reversal(crate::contexts::loans::public::RequestLoanReversal {
            user_id: user,
            agreement_id: LoanAgreementId::new(id),
            movement_id: LoanMovementId::new(movement),
            reason: body.reason,
            expected_version: body.expected_version,
            idempotency_key: key,
            correlation_id: CorrelationId::generate(),
            occurred_at: Utc::now(),
        })
        .await
        .map_err(map_error)?;
    Ok((
        StatusCode::ACCEPTED,
        Json(serde_json::to_value(result).map_err(|_| V2ApiError::internal())?),
    ))
}
pub(crate) async fn replace(
    State(state): State<LoansApiState>,
    AuthenticatedUser(user): AuthenticatedUser,
    Path((id, original)): Path<(uuid::Uuid, uuid::Uuid)>,
    headers: HeaderMap,
    V2Json(body): V2Json<ReplacementBody>,
) -> Result<(StatusCode, Json<serde_json::Value>), V2ApiError> {
    let key = idempotency(&headers)?;
    let (currency, minor_unit) = currency(&state, &body.currency).await?;
    let kind = match body.kind.as_str() {
        "disbursement" => MovementKind::Disbursement,
        "repayment" => MovementKind::Repayment,
        "accrual" => MovementKind::Accrual,
        "write_off" => MovementKind::WriteOff,
        _ => return Err(V2ApiError::bad_request("invalid movement kind")),
    };
    let amounts = MovementAmounts {
        principal: money_decimal(&body.principal, minor_unit)?,
        accrued_interest: money_decimal(&body.accrued_interest, minor_unit)?,
        accrued_fee: money_decimal(&body.accrued_fee, minor_unit)?,
        current_interest: money_decimal(&body.current_interest, minor_unit)?,
        current_fee: money_decimal(&body.current_fee, minor_unit)?,
    };
    let result = state
        .loans
        .request_replacement(
            RecordLoanMovement {
                user_id: user,
                agreement_id: LoanAgreementId::new(id),
                kind,
                currency,
                amounts,
                cash_account_id: body
                    .cash_account_id
                    .map(crate::contexts::ledger::public::LedgerAccountId::new),
                reason: body.reason,
                replaces: Some(LoanMovementId::new(original)),
                expected_version: body.expected_version,
                idempotency_key: key,
                correlation_id: CorrelationId::generate(),
                occurred_at: Utc::now(),
            },
            LoanMovementId::new(original),
        )
        .await
        .map_err(map_error)?;
    Ok((
        StatusCode::ACCEPTED,
        Json(serde_json::to_value(result).map_err(|_| V2ApiError::internal())?),
    ))
}

async fn currency(state: &LoansApiState, value: &str) -> Result<(CurrencyCode, u32), V2ApiError> {
    let code = CurrencyCode::new(value).map_err(|_| V2ApiError::bad_request("invalid currency"))?;
    let definition = state
        .currencies
        .require_enabled(code.clone())
        .await
        .map_err(|_| V2ApiError::bad_request("currency is not enabled"))?;
    Ok((code, u32::from(definition.minor_unit)))
}
fn money_decimal(value: &str, minor_unit: u32) -> Result<Decimal, V2ApiError> {
    if value.is_empty() || value.contains(['e', 'E']) {
        return Err(V2ApiError::bad_request("invalid decimal string"));
    }
    let parsed =
        Decimal::from_str(value).map_err(|_| V2ApiError::bad_request("invalid decimal string"))?;
    if parsed.scale() > minor_unit {
        return Err(V2ApiError::bad_request(
            "decimal scale exceeds currency minor unit",
        ));
    }
    Ok(parsed)
}
fn rate_decimal(value: &str) -> Result<Decimal, V2ApiError> {
    if value.is_empty() || value.contains(['e', 'E']) {
        return Err(V2ApiError::bad_request("invalid decimal string"));
    }
    let parsed =
        Decimal::from_str(value).map_err(|_| V2ApiError::bad_request("invalid decimal string"))?;
    if parsed.scale() > 10 {
        return Err(V2ApiError::bad_request("annual rate scale exceeds limit"));
    }
    Ok(parsed)
}
fn idempotency(headers: &HeaderMap) -> Result<IdempotencyKey, V2ApiError> {
    let raw = headers
        .get("Idempotency-Key")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| V2ApiError::bad_request("Idempotency-Key is required"))?;
    IdempotencyKey::new(raw).map_err(|_| V2ApiError::bad_request("invalid Idempotency-Key"))
}
fn map_error(error: crate::contexts::loans::public::LoansError) -> V2ApiError {
    if error.is_not_found() {
        V2ApiError::not_found("loan was not found")
    } else if error.is_conflict() {
        V2ApiError::conflict("loan command conflict")
    } else if error.is_invalid() {
        V2ApiError::bad_request("invalid loan command")
    } else {
        V2ApiError::internal()
    }
}
