//! Sharing task handlers.

use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use chrono::Utc;
use serde::Deserialize;
use serde_json::json;
use std::str::FromStr;

use super::{dto::*, routes::SharingApiState};
use crate::api::v2::{AuthenticatedUser, V2ApiError, V2Json};
use crate::contexts::reference_data::public::CurrencyCatalog;
use crate::contexts::sharing::application::commands::*;
use crate::contexts::sharing::domain::*;
use crate::contexts::sharing::public::SharingError;
use crate::shared_kernel::{CorrelationId, CurrencyCode, IdempotencyKey, Money};

fn metadata(
    user_id: crate::shared_kernel::UserId,
    headers: &HeaderMap,
    hash: [u8; 32],
    occurred_at: chrono::DateTime<Utc>,
) -> Result<CommandMetadata, V2ApiError> {
    let value = headers
        .get("Idempotency-Key")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| V2ApiError::bad_request("missing Idempotency-Key"))?;
    Ok(CommandMetadata {
        user_id,
        idempotency_key: IdempotencyKey::new(value)
            .map_err(|_| V2ApiError::bad_request("invalid Idempotency-Key"))?,
        request_hash: hash,
        correlation_id: CorrelationId::generate(),
        occurred_at,
    })
}

pub(crate) async fn create_contact(
    State(state): State<SharingApiState>,
    AuthenticatedUser(user): AuthenticatedUser,
    headers: HeaderMap,
    V2Json(body): V2Json<ContactBody>,
) -> Result<Response, V2ApiError> {
    let hash =
        canonical_request_hash(&body).map_err(|_| V2ApiError::bad_request("invalid request"))?;
    let command = CreateContact {
        metadata: metadata(user, &headers, hash, Utc::now())?,
        name: ContactName::new(body.display_name).map_err(map_domain)?,
        note: body.note,
    };
    let result = state
        .sharing
        .create_contact(command)
        .await
        .map_err(map_domain)?;
    Ok((StatusCode::CREATED, Json(result)).into_response())
}
pub(crate) async fn update_contact(
    State(state): State<SharingApiState>,
    AuthenticatedUser(user): AuthenticatedUser,
    Path(id): Path<uuid::Uuid>,
    headers: HeaderMap,
    V2Json(body): V2Json<ContactBody>,
) -> Result<Response, V2ApiError> {
    let expected = body
        .expected_version
        .ok_or_else(|| V2ApiError::bad_request("missing expected_version"))?;
    let hash = canonical_request_hash(&(id, &body))
        .map_err(|_| V2ApiError::bad_request("invalid request"))?;
    let command = UpdateContact {
        metadata: metadata(user, &headers, hash, Utc::now())?,
        contact_id: ContactId::new(id),
        name: ContactName::new(body.display_name).map_err(map_domain)?,
        note: body.note,
        expected_version: ContactVersion(expected),
    };
    Ok(Json(
        state
            .sharing
            .update_contact(command)
            .await
            .map_err(map_domain)?,
    )
    .into_response())
}
pub(crate) async fn archive_contact(
    State(state): State<SharingApiState>,
    AuthenticatedUser(user): AuthenticatedUser,
    Path(id): Path<uuid::Uuid>,
    headers: HeaderMap,
    V2Json(body): V2Json<ArchiveBody>,
) -> Result<Response, V2ApiError> {
    let hash = canonical_request_hash(&(id, &body))
        .map_err(|_| V2ApiError::bad_request("invalid request"))?;
    let command = ArchiveContact {
        metadata: metadata(user, &headers, hash, Utc::now())?,
        contact_id: ContactId::new(id),
        expected_version: ContactVersion(body.expected_version),
    };
    Ok(Json(
        state
            .sharing
            .archive_contact(command)
            .await
            .map_err(map_domain)?,
    )
    .into_response())
}

#[derive(Deserialize)]
pub(crate) struct ContactQuery {
    #[serde(default)]
    include_archived: bool,
}
pub(crate) async fn list_contacts(
    State(state): State<SharingApiState>,
    AuthenticatedUser(user): AuthenticatedUser,
    Query(query): Query<ContactQuery>,
) -> Result<Json<serde_json::Value>, V2ApiError> {
    state
        .sharing
        .contacts(user, query.include_archived)
        .await
        .map(|contacts| Json(json!({"contacts":contacts})))
        .map_err(map_domain)
}
pub(crate) async fn get_contact(
    State(state): State<SharingApiState>,
    AuthenticatedUser(user): AuthenticatedUser,
    Path(id): Path<uuid::Uuid>,
) -> Result<Json<serde_json::Value>, V2ApiError> {
    state
        .sharing
        .contact(user, ContactId::new(id))
        .await
        .map_err(map_domain)?
        .map(|contact| Json(json!(contact)))
        .ok_or_else(|| V2ApiError::not_found("contact not found"))
}

pub(crate) async fn create_bill(
    State(state): State<SharingApiState>,
    AuthenticatedUser(user): AuthenticatedUser,
    headers: HeaderMap,
    V2Json(body): V2Json<BillBody>,
) -> Result<Response, V2ApiError> {
    let hash =
        canonical_request_hash(&body).map_err(|_| V2ApiError::bad_request("invalid request"))?;
    let occurred_at = body.occurred_at;
    let draft = draft(&state, &body).await?;
    let command = CreateBillSplit {
        metadata: metadata(user, &headers, hash, occurred_at)?,
        draft,
    };
    let result = state
        .sharing
        .create_bill(command)
        .await
        .map_err(map_domain)?;
    Ok((StatusCode::ACCEPTED, Json(result)).into_response())
}
pub(crate) async fn revise_bill(
    State(state): State<SharingApiState>,
    AuthenticatedUser(user): AuthenticatedUser,
    Path(id): Path<uuid::Uuid>,
    headers: HeaderMap,
    V2Json(body): V2Json<RevisionBody>,
) -> Result<Response, V2ApiError> {
    let hash = canonical_request_hash(&(id, &body))
        .map_err(|_| V2ApiError::bad_request("invalid request"))?;
    let occurred_at = body.bill.occurred_at;
    let draft = draft(&state, &body.bill).await?;
    let command = ReviseBillSplit {
        metadata: metadata(user, &headers, hash, occurred_at)?,
        bill_id: BillSplitId::new(id),
        expected_version: BillVersion(body.expected_version),
        draft,
    };
    Ok((
        StatusCode::ACCEPTED,
        Json(
            state
                .sharing
                .revise_bill(command)
                .await
                .map_err(map_domain)?,
        ),
    )
        .into_response())
}
pub(crate) async fn cancel_bill(
    State(state): State<SharingApiState>,
    AuthenticatedUser(user): AuthenticatedUser,
    Path(id): Path<uuid::Uuid>,
    headers: HeaderMap,
    V2Json(body): V2Json<CancellationBody>,
) -> Result<Response, V2ApiError> {
    let hash = canonical_request_hash(&(id, &body))
        .map_err(|_| V2ApiError::bad_request("invalid request"))?;
    let command = CancelBillSplit {
        metadata: metadata(user, &headers, hash, Utc::now())?,
        bill_id: BillSplitId::new(id),
        expected_version: BillVersion(body.expected_version),
        reason: body.reason,
    };
    Ok((
        StatusCode::ACCEPTED,
        Json(
            state
                .sharing
                .cancel_bill(command)
                .await
                .map_err(map_domain)?,
        ),
    )
        .into_response())
}
pub(crate) async fn list_bills(
    State(state): State<SharingApiState>,
    AuthenticatedUser(user): AuthenticatedUser,
) -> Result<Json<serde_json::Value>, V2ApiError> {
    state
        .sharing
        .bills(user)
        .await
        .map(|bills| Json(json!({"bill_splits":bills})))
        .map_err(map_domain)
}
pub(crate) async fn get_bill(
    State(state): State<SharingApiState>,
    AuthenticatedUser(user): AuthenticatedUser,
    Path(id): Path<uuid::Uuid>,
) -> Result<Json<serde_json::Value>, V2ApiError> {
    state
        .sharing
        .bill(user, BillSplitId::new(id))
        .await
        .map_err(map_domain)?
        .map(|bill| Json(json!(bill)))
        .ok_or_else(|| V2ApiError::not_found("bill split not found"))
}

pub(crate) async fn create_settlement(
    State(state): State<SharingApiState>,
    AuthenticatedUser(user): AuthenticatedUser,
    Path(id): Path<uuid::Uuid>,
    headers: HeaderMap,
    V2Json(body): V2Json<SettlementBody>,
) -> Result<Response, V2ApiError> {
    let hash = canonical_request_hash(&(id, &body))
        .map_err(|_| V2ApiError::bad_request("invalid request"))?;
    let (amount, _) = money(&state, &body.amount).await?;
    let evidence = match body.evidence {
        SettlementEvidenceDto::External => SettlementEvidence::External,
        SettlementEvidenceDto::Manual { account_id } => SettlementEvidence::Manual {
            account_id: LedgerAccountReference::new(account_id),
        },
        SettlementEvidenceDto::ExistingJournal { journal_id } => {
            SettlementEvidence::ExistingJournal {
                journal_id: LedgerJournalReference::new(journal_id),
            }
        }
    };
    let command = CreateSettlement {
        metadata: metadata(user, &headers, hash, body.occurred_at)?,
        bill_id: BillSplitId::new(id),
        expected_version: BillVersion(body.expected_version),
        debtor: participant(body.debtor),
        creditor: participant(body.creditor),
        amount,
        evidence,
    };
    Ok((
        StatusCode::ACCEPTED,
        Json(
            state
                .sharing
                .create_settlement(command)
                .await
                .map_err(map_domain)?,
        ),
    )
        .into_response())
}
pub(crate) async fn reverse_settlement(
    State(state): State<SharingApiState>,
    AuthenticatedUser(user): AuthenticatedUser,
    Path((id, settlement_id)): Path<(uuid::Uuid, uuid::Uuid)>,
    headers: HeaderMap,
    V2Json(body): V2Json<ReversalBody>,
) -> Result<Response, V2ApiError> {
    let hash = canonical_request_hash(&(id, settlement_id, &body))
        .map_err(|_| V2ApiError::bad_request("invalid request"))?;
    let command = ReverseSettlement {
        metadata: metadata(user, &headers, hash, Utc::now())?,
        bill_id: BillSplitId::new(id),
        settlement_id: SettlementId::new(settlement_id),
        expected_version: SettlementVersion(body.expected_version),
        reason: body.reason,
    };
    Ok((
        StatusCode::ACCEPTED,
        Json(
            state
                .sharing
                .reverse_settlement(command)
                .await
                .map_err(map_domain)?,
        ),
    )
        .into_response())
}

async fn draft(state: &SharingApiState, body: &BillBody) -> Result<BillDraft, V2ApiError> {
    let (total, scale) = money(state, &body.total).await?;
    let mut contributions = Vec::with_capacity(body.contributions.len());
    for value in &body.contributions {
        let (amount, _) = money(state, &value.amount).await?;
        let evidence = match &value.evidence {
            ContributionEvidenceDto::External => ContributionEvidence::External,
            ContributionEvidenceDto::Manual { account_id } => ContributionEvidence::Manual {
                account_id: LedgerAccountReference::new(*account_id),
            },
            ContributionEvidenceDto::ExistingJournals { allocations } => {
                let mut result = Vec::with_capacity(allocations.len());
                for item in allocations {
                    let (amount, _) = money(state, &item.amount).await?;
                    result.push(JournalAllocation {
                        journal_id: LedgerJournalReference::new(item.journal_id),
                        amount,
                    });
                }
                ContributionEvidence::ExistingJournals {
                    allocations: result,
                }
            }
        };
        contributions.push(
            Contribution::new(participant(value.participant.clone()), amount, evidence)
                .map_err(map_domain)?,
        );
    }
    let shares = match &body.shares {
        SharesDto::Equal { participants } => {
            ShareRequest::Equal(participants.iter().cloned().map(participant).collect())
        }
        SharesDto::Exact { shares } => {
            let mut result = Vec::with_capacity(shares.len());
            for value in shares {
                let (amount, _) = money(state, &value.amount).await?;
                result.push(ExactShare {
                    participant: participant(value.participant.clone()),
                    amount,
                });
            }
            ShareRequest::Exact(result)
        }
    };
    Ok(BillDraft {
        title: body.title.clone(),
        occurred_at: body.occurred_at,
        total,
        minor_unit_scale: scale,
        contributions,
        shares,
    })
}
async fn money(state: &SharingApiState, value: &MoneyDto) -> Result<(Money, u32), V2ApiError> {
    let code = CurrencyCode::new(&value.currency)
        .map_err(|_| V2ApiError::bad_request("invalid currency"))?;
    let definition = state
        .currencies
        .require_enabled(code.clone())
        .await
        .map_err(|_| V2ApiError::bad_request("currency is not enabled"))?;
    let amount = rust_decimal::Decimal::from_str(&value.amount)
        .map_err(|_| V2ApiError::bad_request("invalid money amount"))?;
    Ok((
        Money::new(amount, code, u32::from(definition.minor_unit))
            .map_err(|_| V2ApiError::bad_request("invalid money amount"))?,
        u32::from(definition.minor_unit),
    ))
}
fn participant(value: ParticipantDto) -> Participant {
    match value {
        ParticipantDto::CurrentUser => Participant::CurrentUser,
        ParticipantDto::Contact(id) => Participant::Contact(ContactId::new(id)),
    }
}
fn map_domain(error: SharingError) -> V2ApiError {
    match error {
        SharingError::NotFound => V2ApiError::not_found("sharing item not found"),
        SharingError::VersionConflict { .. }
        | SharingError::IdempotencyConflict
        | SharingError::ActiveSettlements
        | SharingError::AccountingPending => V2ApiError::conflict("sharing conflict"),
        SharingError::Persistence(_) => V2ApiError::internal(),
        _ => V2ApiError::bad_request("invalid sharing command"),
    }
}
