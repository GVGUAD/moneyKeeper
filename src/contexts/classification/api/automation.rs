//! HTTP workflows for review, retry, and historical classification.

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::api::{ApiError, ApiJson, AuthenticatedUser};
use crate::contexts::classification::public::{
    AutomationError, BackfillJobId, BackfillJobView, BackfillRange, CategoryCatalog,
    CategoryCatalogFacade, CategoryId, CategoryKind, ClassificationAutomationFacade,
    ClassificationDecisionId, DecisionSuggestion, ReviewAction, ReviewCursor, ReviewItem,
};
use crate::contexts::ledger::public::{
    AccountNature, AnnotationVersion, EnableAutomaticClassification, JournalEntryId, LedgerFacade,
};
use crate::shared_kernel::IdempotencyKey;

#[derive(Clone)]
pub(crate) struct ClassificationApiState {
    pub(crate) automation: ClassificationAutomationFacade,
    pub(crate) categories: CategoryCatalogFacade,
    pub(crate) ledger: LedgerFacade,
}

pub(crate) fn router(state: ClassificationApiState) -> Router {
    Router::new()
        .route("/classification/review-queue", get(review_queue))
        .route(
            "/classification/decisions/{id}/resolve",
            post(resolve_decision),
        )
        .route(
            "/transactions/{id}/classification/retry",
            post(retry_transaction),
        )
        .route("/classification/backfills", post(start_backfill))
        .route("/classification/backfills/{id}", get(get_backfill))
        .with_state(state)
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReviewQueueQuery {
    after_created_at: Option<DateTime<Utc>>,
    after_decision_id: Option<Uuid>,
    limit: Option<u32>,
}

#[derive(Debug, Serialize)]
struct ReviewQueueResponse {
    items: Vec<ReviewItemResponse>,
    next_cursor: Option<ReviewCursorResponse>,
}

#[derive(Debug, Serialize)]
struct ReviewCursorResponse {
    created_at: DateTime<Utc>,
    decision_id: Uuid,
}

#[derive(Debug, Serialize)]
struct ReviewItemResponse {
    decision_id: Uuid,
    decision_version: i64,
    journal_entry_id: Uuid,
    candidate_category_id: Uuid,
    confidence: f64,
    reason_code: &'static str,
    explanation: String,
    taxonomy_version: i64,
    annotation_version: i64,
    created_at: DateTime<Utc>,
}

impl From<ReviewItem> for ReviewItemResponse {
    fn from(value: ReviewItem) -> Self {
        Self {
            decision_id: value.decision_id.into_uuid(),
            decision_version: value.decision_version,
            journal_entry_id: value.journal_entry_id,
            candidate_category_id: value.candidate_category_id,
            confidence: value.confidence.as_f64(),
            reason_code: value.reason.as_str(),
            explanation: value.explanation,
            taxonomy_version: value.taxonomy_version,
            annotation_version: value.annotation_version,
            created_at: value.created_at,
        }
    }
}

async fn review_queue(
    AuthenticatedUser(user_id): AuthenticatedUser,
    State(state): State<ClassificationApiState>,
    Query(query): Query<ReviewQueueQuery>,
) -> Result<Json<ReviewQueueResponse>, ApiError> {
    let cursor = match (query.after_created_at, query.after_decision_id) {
        (None, None) => None,
        (Some(created_at), Some(id)) => Some(ReviewCursor {
            created_at,
            decision_id: ClassificationDecisionId::new(id),
        }),
        _ => {
            return Err(ApiError::bad_request(
                "review cursor requires after_created_at and after_decision_id",
            ));
        }
    };
    let page = state
        .automation
        .review_queue(user_id, cursor, query.limit.unwrap_or(50))
        .await
        .map_err(map_automation_error)?;
    state
        .categories
        .taxonomy(user_id, Utc::now())
        .await
        .map_err(super::handlers::map_error)?;
    Ok(Json(ReviewQueueResponse {
        items: page
            .items
            .into_iter()
            .map(ReviewItemResponse::from)
            .collect(),
        next_cursor: page.next_cursor.map(|value| ReviewCursorResponse {
            created_at: value.created_at,
            decision_id: value.decision_id.into_uuid(),
        }),
    }))
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ResolveAction {
    Accept,
    Correct,
    Reject,
}

impl From<ResolveAction> for ReviewAction {
    fn from(value: ResolveAction) -> Self {
        match value {
            ResolveAction::Accept => Self::Accept,
            ResolveAction::Correct => Self::Correct,
            ResolveAction::Reject => Self::Reject,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResolveDecisionRequest {
    action: ResolveAction,
    #[serde(default)]
    category_id: Option<Uuid>,
    expected_decision_version: i64,
    expected_annotation_version: i64,
}

async fn resolve_decision(
    AuthenticatedUser(user_id): AuthenticatedUser,
    State(state): State<ClassificationApiState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    ApiJson(request): ApiJson<ResolveDecisionRequest>,
) -> Result<(StatusCode, Json<DecisionSuggestion>), ApiError> {
    let action = ReviewAction::from(request.action);
    let decision_id = ClassificationDecisionId::new(id);
    let idempotency_key = idempotency_key(&headers)?;
    if let Some(replayed) = state
        .automation
        .replay_review_resolution(
            user_id,
            decision_id,
            action,
            request.category_id,
            request.expected_decision_version,
            request.expected_annotation_version,
            &idempotency_key,
        )
        .await
        .map_err(map_automation_error)?
    {
        return Ok((StatusCode::ACCEPTED, Json(replayed)));
    }
    let suggestion = state
        .automation
        .get_decision(user_id, decision_id)
        .await
        .map_err(map_automation_error)?;
    let journal = state
        .ledger
        .get_journal(user_id, JournalEntryId::new(suggestion.journal_entry_id))
        .await
        .map_err(map_ledger_error)?;
    let current = journal
        .annotation
        .as_ref()
        .ok_or_else(|| ApiError::conflict("transaction is not classifiable"))?;
    if current.version.get() != request.expected_annotation_version {
        return Err(ApiError::conflict("annotation version conflict"));
    }
    if current.category_id.is_some()
        || current.automation_state != crate::contexts::ledger::public::AutomationState::Eligible
    {
        return Err(ApiError::conflict("transaction is no longer classifiable"));
    }
    let taxonomy = state
        .categories
        .taxonomy(user_id, Utc::now())
        .await
        .map_err(|_| ApiError::conflict("classification taxonomy changed"))?;
    if taxonomy.version != suggestion.taxonomy_version {
        return Err(ApiError::conflict("classification decision is stale"));
    }
    let chosen = match action {
        ReviewAction::Accept => suggestion.candidate_category_id,
        ReviewAction::Correct => request.category_id,
        ReviewAction::Reject => None,
    };
    if matches!(action, ReviewAction::Accept | ReviewAction::Correct) {
        let category_id = chosen.ok_or_else(|| {
            ApiError::bad_request("accept or correct requires a category candidate")
        })?;
        state
            .categories
            .require_assignable(
                user_id,
                CategoryId::new(category_id),
                cash_flow_kind(&journal)?,
            )
            .await
            .map_err(|_| ApiError::conflict("category is not assignable"))?;
    }
    let result = state
        .automation
        .resolve_review(
            user_id,
            decision_id,
            action,
            request.category_id,
            request.expected_decision_version,
            request.expected_annotation_version,
            &idempotency_key,
            Utc::now(),
        )
        .await
        .map_err(map_automation_error)?;
    Ok((StatusCode::ACCEPTED, Json(result)))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RetryRequest {
    expected_annotation_version: i64,
}

async fn retry_transaction(
    AuthenticatedUser(user_id): AuthenticatedUser,
    State(state): State<ClassificationApiState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    ApiJson(request): ApiJson<RetryRequest>,
) -> Result<
    (
        StatusCode,
        Json<crate::contexts::ledger::public::CategoryAssignmentResult>,
    ),
    ApiError,
> {
    let now = Utc::now();
    state
        .categories
        .taxonomy(user_id, now)
        .await
        .map_err(super::handlers::map_error)?;
    let result = state
        .ledger
        .enable_automatic_classification(EnableAutomaticClassification {
            user_id,
            journal_entry_id: JournalEntryId::new(id),
            expected_version: AnnotationVersion::new(request.expected_annotation_version)
                .map_err(map_ledger_error)?,
            idempotency_key: idempotency_key(&headers)?,
            correlation_id: crate::api::request_correlation_id(),
            occurred_at: now,
        })
        .await
        .map_err(map_ledger_error)?;
    Ok((StatusCode::ACCEPTED, Json(result)))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StartBackfillRequest {
    from: DateTime<Utc>,
    to: DateTime<Utc>,
}

async fn start_backfill(
    AuthenticatedUser(user_id): AuthenticatedUser,
    State(state): State<ClassificationApiState>,
    headers: HeaderMap,
    ApiJson(request): ApiJson<StartBackfillRequest>,
) -> Result<(StatusCode, Json<BackfillJobView>), ApiError> {
    let range = BackfillRange::new(request.from, request.to)
        .map_err(|_| ApiError::bad_request("backfill range must be half-open and non-empty"))?;
    state
        .categories
        .taxonomy(user_id, Utc::now())
        .await
        .map_err(super::handlers::map_error)?;
    let result = state
        .automation
        .start_backfill(user_id, range, &idempotency_key(&headers)?, Utc::now())
        .await
        .map_err(map_automation_error)?;
    Ok((StatusCode::ACCEPTED, Json(result)))
}

async fn get_backfill(
    AuthenticatedUser(user_id): AuthenticatedUser,
    State(state): State<ClassificationApiState>,
    Path(id): Path<Uuid>,
) -> Result<Json<BackfillJobView>, ApiError> {
    state
        .automation
        .get_backfill(user_id, BackfillJobId::new(id))
        .await
        .map(Json)
        .map_err(map_automation_error)
}

fn cash_flow_kind(
    journal: &crate::contexts::ledger::public::JournalView,
) -> Result<CategoryKind, ApiError> {
    let mut kind = None;
    for posting in &journal.postings {
        let candidate = match posting.account_nature {
            AccountNature::Income => Some(CategoryKind::Income),
            AccountNature::Expense => Some(CategoryKind::Expense),
            _ => None,
        };
        if let Some(candidate) = candidate {
            if kind.is_some_and(|current| current != candidate) {
                return Err(ApiError::conflict(
                    "transaction cash-flow kind is ambiguous",
                ));
            }
            kind = Some(candidate);
        }
    }
    kind.ok_or_else(|| ApiError::conflict("transaction is not an income or expense cash flow"))
}

fn idempotency_key(headers: &HeaderMap) -> Result<IdempotencyKey, ApiError> {
    let value = headers
        .get("Idempotency-Key")
        .ok_or_else(|| ApiError::bad_request("missing Idempotency-Key header"))?
        .to_str()
        .map_err(|_| ApiError::bad_request("invalid Idempotency-Key header"))?;
    IdempotencyKey::new(value).map_err(|_| ApiError::bad_request("invalid Idempotency-Key header"))
}

fn map_automation_error(error: AutomationError) -> ApiError {
    if error.is_not_found() {
        ApiError::not_found("classification resource was not found")
    } else if error.is_invalid() {
        ApiError::bad_request("invalid classification request")
    } else if error.is_conflict() {
        ApiError::conflict("classification state conflict")
    } else {
        ApiError::internal(
            "classification.persistence",
            "classification storage operation failed",
        )
    }
}

fn map_ledger_error(error: crate::contexts::ledger::public::LedgerError) -> ApiError {
    if error.is_not_found() {
        ApiError::not_found("transaction was not found")
    } else if error.is_version_conflict() || error.is_invalid_annotation() {
        ApiError::conflict("transaction classification conflict")
    } else {
        ApiError::internal("ledger.command", "Ledger operation failed")
    }
}
