//! Transfer conversion HTTP contracts.
use super::handlers::{idempotency_key, map_ledger_error};
use crate::api::{ApiError, ApiJson, AuthenticatedUser, state::LedgerApiState};
use crate::contexts::ledger::public::*;
use axum::{
    Json,
    extract::{Path, Query, State},
    http::HeaderMap,
};
use serde::Deserialize;
use uuid::Uuid;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ConvertRequest {
    #[serde(flatten)]
    pub input: ConversionInput,
    pub version_token: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EditRequest {
    pub expected_version: i64,
    pub title: String,
    pub note: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct UndoRequest {
    pub expected_version: i64,
}
async fn validate_money(state: &LedgerApiState, input: &ConversionInput) -> Result<(), ApiError> {
    for money in input.missing_side.iter().chain(input.fee.iter()) {
        super::handlers::money_for(state, money.amount.to_string(), money.currency.clone()).await?;
    }
    Ok(())
}
pub(crate) async fn candidates(
    AuthenticatedUser(user): AuthenticatedUser,
    State(s): State<LedgerApiState>,
    Path(id): Path<Uuid>,
    Query(query): Query<ConversionCandidatesQuery>,
) -> Result<Json<ConversionResponse>, ApiError> {
    s.ledger
        .transfer_conversion(
            user,
            ConversionAction::Candidates {
                journal_id: JournalEntryId::new(id),
                query,
            },
        )
        .await
        .map(Json)
        .map_err(map_conversion_error)
}
pub(crate) async fn preview(
    AuthenticatedUser(user): AuthenticatedUser,
    State(s): State<LedgerApiState>,
    Path(id): Path<Uuid>,
    ApiJson(input): ApiJson<ConversionInput>,
) -> Result<Json<ConversionResponse>, ApiError> {
    validate_money(&s, &input).await?;
    s.ledger
        .transfer_conversion(
            user,
            ConversionAction::Preview {
                journal_id: JournalEntryId::new(id),
                input,
            },
        )
        .await
        .map(Json)
        .map_err(map_conversion_error)
}
pub(crate) async fn convert(
    AuthenticatedUser(user): AuthenticatedUser,
    State(s): State<LedgerApiState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    ApiJson(r): ApiJson<ConvertRequest>,
) -> Result<Json<ConversionResponse>, ApiError> {
    validate_money(&s, &r.input).await?;
    s.ledger
        .transfer_conversion(
            user,
            ConversionAction::Convert {
                journal_id: JournalEntryId::new(id),
                input: r.input,
                version_token: r.version_token,
                key: idempotency_key(&headers)?,
            },
        )
        .await
        .map(Json)
        .map_err(map_conversion_error)
}
pub(crate) async fn get(
    AuthenticatedUser(user): AuthenticatedUser,
    State(s): State<LedgerApiState>,
    Path(id): Path<Uuid>,
) -> Result<Json<ConversionResponse>, ApiError> {
    s.ledger
        .transfer_conversion(user, ConversionAction::Get { id })
        .await
        .map(Json)
        .map_err(map_conversion_error)
}
pub(crate) async fn edit(
    AuthenticatedUser(user): AuthenticatedUser,
    State(s): State<LedgerApiState>,
    Path(id): Path<Uuid>,
    ApiJson(r): ApiJson<EditRequest>,
) -> Result<Json<ConversionResponse>, ApiError> {
    s.ledger
        .transfer_conversion(
            user,
            ConversionAction::Edit {
                id,
                expected_version: r.expected_version,
                title: r.title,
                note: r.note,
            },
        )
        .await
        .map(Json)
        .map_err(map_conversion_error)
}
pub(crate) async fn undo(
    AuthenticatedUser(user): AuthenticatedUser,
    State(s): State<LedgerApiState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    ApiJson(r): ApiJson<UndoRequest>,
) -> Result<Json<ConversionResponse>, ApiError> {
    s.ledger
        .transfer_conversion(
            user,
            ConversionAction::Undo {
                id,
                expected_version: r.expected_version,
                key: idempotency_key(&headers)?,
            },
        )
        .await
        .map(Json)
        .map_err(map_conversion_error)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReviewRequest {
    pub expected_version: i64,
    pub conversion_id: Option<Uuid>,
}
pub(crate) async fn reviews(
    AuthenticatedUser(user): AuthenticatedUser,
    State(s): State<LedgerApiState>,
) -> Result<Json<ConversionResponse>, ApiError> {
    s.ledger
        .transfer_conversion(user, ConversionAction::Reviews { id: None })
        .await
        .map(Json)
        .map_err(map_conversion_error)
}
pub(crate) async fn review(
    AuthenticatedUser(user): AuthenticatedUser,
    State(s): State<LedgerApiState>,
    Path(id): Path<Uuid>,
) -> Result<Json<ConversionResponse>, ApiError> {
    s.ledger
        .transfer_conversion(user, ConversionAction::Reviews { id: Some(id) })
        .await
        .map(Json)
        .map_err(map_conversion_error)
}
pub(crate) async fn resolve_review(
    AuthenticatedUser(user): AuthenticatedUser,
    State(s): State<LedgerApiState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    ApiJson(r): ApiJson<ReviewRequest>,
) -> Result<Json<ConversionResponse>, ApiError> {
    s.ledger
        .transfer_conversion(
            user,
            ConversionAction::ResolveReview {
                id,
                conversion_id: r.conversion_id,
                expected_version: r.expected_version,
                key: idempotency_key(&headers)?,
            },
        )
        .await
        .map(Json)
        .map_err(map_conversion_error)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AttachRequest {
    pub journal_id: JournalEntryId,
    pub expected_version: i64,
}
pub(crate) async fn attach(
    AuthenticatedUser(user): AuthenticatedUser,
    State(s): State<LedgerApiState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    ApiJson(r): ApiJson<AttachRequest>,
) -> Result<Json<ConversionResponse>, ApiError> {
    s.ledger
        .transfer_conversion(
            user,
            ConversionAction::Attach {
                id,
                journal_id: r.journal_id,
                expected_version: r.expected_version,
                key: idempotency_key(&headers)?,
            },
        )
        .await
        .map(Json)
        .map_err(map_conversion_error)
}

pub(crate) async fn notifications(
    AuthenticatedUser(user): AuthenticatedUser,
    State(s): State<LedgerApiState>,
) -> Result<Json<ConversionResponse>, ApiError> {
    s.ledger
        .transfer_conversion(user, ConversionAction::Notifications)
        .await
        .map(Json)
        .map_err(map_conversion_error)
}

pub(crate) async fn attachment_candidates(
    AuthenticatedUser(user): AuthenticatedUser,
    State(s): State<LedgerApiState>,
    Path(id): Path<Uuid>,
    Query(query): Query<ConversionCandidatesQuery>,
) -> Result<Json<ConversionResponse>, ApiError> {
    s.ledger
        .transfer_conversion(user, ConversionAction::AttachmentCandidates { id, query })
        .await
        .map(Json)
        .map_err(map_conversion_error)
}

fn map_conversion_error(error: LedgerError) -> ApiError {
    if error.is_invalid_state() {
        ApiError::bad_request(
            "Choose live ordinary manual or imported income/expense transactions showing opposite movements in different active accounts. Resolve linked workflows before converting.",
        )
    } else if error.is_invalid_money() {
        ApiError::bad_request(
            "Amounts must preserve the recorded movements. For the same currency, incoming cannot exceed outgoing; supply and confirm the exact difference as a fee. FX fees must use a represented currency.",
        )
    } else {
        map_ledger_error(error)
    }
}
