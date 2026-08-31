use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use chrono::Utc;
use rust_decimal::Decimal;
use uuid::Uuid;

use crate::api::state::LedgerApiState;
use crate::api::{ApiError, ApiJson, AuthenticatedUser};
use crate::contexts::classification::public::CategoryId;
use crate::contexts::ledger::public::{
    AccountVersion, ActivityCursor, ActivityFilter, ActivityKind, AnnotationChanges,
    AnnotationVersion, ApproveReconciliation, ArchiveAccount, BalanceVersion, CategoryReference,
    CorrectBalance, DismissReconciliation, JournalEntryId, LedgerAccountId, LedgerError,
    NormalizedTags, OpenAccount, ReconciliationCaseId, ReconciliationVersion,
    RecordManualTransaction, RenameAccount, ReplaceTransaction, RestoreAccount, ReverseTransaction,
    TransferFee, TransferFunds, UpdateTransactionAnnotation,
};
use crate::contexts::reference_data::public::{CurrencyCatalog, CurrencyError};
use crate::shared_kernel::{CurrencyCode, IdempotencyKey, Money};

use super::dto::{
    ActivityQuery, ActivitySummaryQuery, AnnotationRequest, ApproveReconciliationRequest,
    BalanceCorrectionRequest, DismissReconciliationRequest, ExpectedAccountVersionRequest,
    MoneyRequest, OpenAccountRequest, RecordTransactionRequest, RenameAccountRequest,
    ReplaceRequest, ReverseRequest, TransactionActivityQuery, TransferRequest,
};

pub(crate) async fn open_account(
    AuthenticatedUser(user_id): AuthenticatedUser,
    State(state): State<LedgerApiState>,
    headers: HeaderMap,
    ApiJson(request): ApiJson<OpenAccountRequest>,
) -> Result<
    (
        StatusCode,
        Json<crate::contexts::ledger::public::AccountResult>,
    ),
    ApiError,
> {
    let key = idempotency_key(&headers)?;
    let currency = currency(&request.currency)?;
    let money = money_for(&state, request.opening_balance, currency.clone()).await?;
    let result = state
        .ledger
        .open_account(OpenAccount {
            user_id,
            name: request.name,
            currency,
            kind: request.kind,
            nature: request.nature,
            opening_balance: money,
            idempotency_key: key,
            correlation_id: crate::api::request_correlation_id(),
            causation_id: None,
            occurred_at: request.occurred_at.unwrap_or_else(Utc::now),
        })
        .await
        .map_err(map_ledger_error)?;
    Ok((StatusCode::CREATED, Json(result)))
}

pub(crate) async fn list_accounts(
    AuthenticatedUser(user_id): AuthenticatedUser,
    State(state): State<LedgerApiState>,
) -> Result<Json<Vec<crate::contexts::ledger::public::AccountView>>, ApiError> {
    state
        .ledger
        .list_accounts(user_id)
        .await
        .map(Json)
        .map_err(map_ledger_error)
}

pub(crate) async fn get_account(
    AuthenticatedUser(user_id): AuthenticatedUser,
    State(state): State<LedgerApiState>,
    Path(id): Path<Uuid>,
) -> Result<Json<crate::contexts::ledger::public::AccountView>, ApiError> {
    let mut account = state
        .ledger
        .get_account(user_id, LedgerAccountId::new(id))
        .await
        .map_err(map_ledger_error)?;
    if let Some(banking) = &state.banking {
        let summary = banking
            .provider_account_summary(user_id, account.id)
            .await
            .map_err(|_| {
                ApiError::internal(
                    "banking.persistence",
                    "provider account summary query failed",
                )
            })?;
        account.provider_reported = summary.provider_reported;
        account.available = summary.available;
        account.reconciliation_difference = summary
            .provider_reported
            .map(|provider| provider - account.display_balance);
    }
    Ok(Json(account))
}

pub(crate) async fn rename_account(
    AuthenticatedUser(user_id): AuthenticatedUser,
    State(state): State<LedgerApiState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    ApiJson(request): ApiJson<RenameAccountRequest>,
) -> Result<Json<crate::contexts::ledger::public::AccountResult>, ApiError> {
    let command = RenameAccount {
        user_id,
        account_id: LedgerAccountId::new(id),
        name: request.name,
        expected_version: account_version(request.expected_version)?,
        idempotency_key: idempotency_key(&headers)?,
        correlation_id: crate::api::request_correlation_id(),
        occurred_at: request.occurred_at.unwrap_or_else(Utc::now),
    };
    state
        .ledger
        .rename_account(command)
        .await
        .map(Json)
        .map_err(map_ledger_error)
}

pub(crate) async fn archive_account(
    AuthenticatedUser(user_id): AuthenticatedUser,
    State(state): State<LedgerApiState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    ApiJson(request): ApiJson<ExpectedAccountVersionRequest>,
) -> Result<Json<crate::contexts::ledger::public::AccountResult>, ApiError> {
    state
        .ledger
        .archive_account(ArchiveAccount {
            user_id,
            account_id: LedgerAccountId::new(id),
            expected_version: account_version(request.expected_version)?,
            idempotency_key: idempotency_key(&headers)?,
            correlation_id: crate::api::request_correlation_id(),
            occurred_at: request.occurred_at.unwrap_or_else(Utc::now),
        })
        .await
        .map(Json)
        .map_err(map_ledger_error)
}

pub(crate) async fn restore_account(
    AuthenticatedUser(user_id): AuthenticatedUser,
    State(state): State<LedgerApiState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    ApiJson(request): ApiJson<ExpectedAccountVersionRequest>,
) -> Result<Json<crate::contexts::ledger::public::AccountResult>, ApiError> {
    state
        .ledger
        .restore_account(RestoreAccount {
            user_id,
            account_id: LedgerAccountId::new(id),
            expected_version: account_version(request.expected_version)?,
            idempotency_key: idempotency_key(&headers)?,
            correlation_id: crate::api::request_correlation_id(),
            occurred_at: request.occurred_at.unwrap_or_else(Utc::now),
        })
        .await
        .map(Json)
        .map_err(map_ledger_error)
}

pub(crate) async fn account_activity(
    AuthenticatedUser(user_id): AuthenticatedUser,
    State(state): State<LedgerApiState>,
    Path(id): Path<Uuid>,
    Query(query): Query<ActivityQuery>,
) -> Result<Json<Vec<crate::contexts::ledger::public::JournalView>>, ApiError> {
    let after = cursor(&query)?;
    state
        .ledger
        .account_activity(
            user_id,
            LedgerAccountId::new(id),
            after,
            query.limit.unwrap_or(50),
        )
        .await
        .map(Json)
        .map_err(map_ledger_error)
}

pub(crate) async fn record_transaction(
    AuthenticatedUser(user_id): AuthenticatedUser,
    State(state): State<LedgerApiState>,
    headers: HeaderMap,
    ApiJson(request): ApiJson<RecordTransactionRequest>,
) -> Result<
    (
        StatusCode,
        Json<crate::contexts::ledger::public::TransactionResult>,
    ),
    ApiError,
> {
    let amount = money(&state, &request.amount).await?;
    let tags = NormalizedTags::new(request.tags).map_err(map_ledger_error)?;
    let result = state
        .ledger
        .record_manual_transaction(RecordManualTransaction {
            user_id,
            account_id: LedgerAccountId::new(request.account_id),
            kind: request.kind,
            amount,
            description: request.description,
            category_id: request.category_id.map(CategoryId::new),
            note: request.note,
            tags,
            budget_visibility: request.budget_visibility,
            idempotency_key: idempotency_key(&headers)?,
            correlation_id: crate::api::request_correlation_id(),
            causation_id: None,
            occurred_at: request.occurred_at.unwrap_or_else(Utc::now),
        })
        .await
        .map_err(map_ledger_error)?;
    Ok((StatusCode::CREATED, Json(result)))
}

pub(crate) async fn list_transactions(
    AuthenticatedUser(user_id): AuthenticatedUser,
    State(state): State<LedgerApiState>,
    Query(query): Query<TransactionActivityQuery>,
) -> Result<Json<Vec<crate::contexts::ledger::public::JournalView>>, ApiError> {
    let after = cursor_values(query.after_occurred_at, query.after_sequence)?;
    let result = match (query.from_occurred_at, query.before_occurred_at, query.kind) {
        (None, None, None) => {
            state
                .ledger
                .list_journals(user_id, after, query.limit.unwrap_or(50))
                .await
        }
        (Some(from), Some(before), kind) => {
            let filter = ActivityFilter::new(from, before, kind.unwrap_or(ActivityKind::All))
                .map_err(map_ledger_error)?;
            state
                .ledger
                .list_activity(user_id, filter, after, query.limit.unwrap_or(50))
                .await
        }
        _ => {
            return Err(ApiError::bad_request(
                "filtered activity requires from_occurred_at and before_occurred_at",
            ));
        }
    };
    result.map(Json).map_err(map_ledger_error)
}

pub(crate) async fn summarize_transactions(
    AuthenticatedUser(user_id): AuthenticatedUser,
    State(state): State<LedgerApiState>,
    Query(query): Query<ActivitySummaryQuery>,
) -> Result<Json<crate::contexts::ledger::public::ActivitySummary>, ApiError> {
    let (Some(from), Some(before)) = (query.from_occurred_at, query.before_occurred_at) else {
        return Err(ApiError::bad_request(
            "activity summary requires from_occurred_at and before_occurred_at",
        ));
    };
    let filter = ActivityFilter::new(from, before, query.kind.unwrap_or(ActivityKind::All))
        .map_err(map_ledger_error)?;
    state
        .ledger
        .summarize_activity(user_id, filter)
        .await
        .map(Json)
        .map_err(map_ledger_error)
}

pub(crate) async fn get_transaction(
    AuthenticatedUser(user_id): AuthenticatedUser,
    State(state): State<LedgerApiState>,
    Path(id): Path<Uuid>,
) -> Result<Json<crate::contexts::ledger::public::JournalView>, ApiError> {
    state
        .ledger
        .get_journal(user_id, JournalEntryId::new(id))
        .await
        .map(Json)
        .map_err(map_ledger_error)
}

pub(crate) async fn update_annotation(
    AuthenticatedUser(user_id): AuthenticatedUser,
    State(state): State<LedgerApiState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    ApiJson(request): ApiJson<AnnotationRequest>,
) -> Result<Json<crate::contexts::ledger::public::AnnotationResult>, ApiError> {
    if request.clear_category && request.category_id.is_some()
        || request.clear_note && request.note.is_some()
    {
        return Err(ApiError::bad_request(
            "clear flags conflict when values are present",
        ));
    }
    let category = if request.clear_category {
        Some(None)
    } else {
        request
            .category_id
            .map(|id| Some(CategoryReference::new(id)))
    };
    let note = if request.clear_note {
        Some(None)
    } else {
        request.note.map(Some)
    };
    let tags = request
        .tags
        .map(NormalizedTags::new)
        .transpose()
        .map_err(map_ledger_error)?;
    state
        .ledger
        .update_annotation(UpdateTransactionAnnotation {
            user_id,
            journal_entry_id: JournalEntryId::new(id),
            changes: AnnotationChanges {
                description: request.description,
                category,
                note,
                tags,
                budget_visibility: request.budget_visibility,
            },
            expected_version: AnnotationVersion::new(request.expected_version)
                .map_err(map_ledger_error)?,
            idempotency_key: idempotency_key(&headers)?,
            correlation_id: crate::api::request_correlation_id(),
            occurred_at: request.occurred_at.unwrap_or_else(Utc::now),
        })
        .await
        .map(Json)
        .map_err(map_ledger_error)
}

pub(crate) async fn reverse_transaction(
    AuthenticatedUser(user_id): AuthenticatedUser,
    State(state): State<LedgerApiState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    ApiJson(request): ApiJson<ReverseRequest>,
) -> Result<
    (
        StatusCode,
        Json<crate::contexts::ledger::public::FinancialChangeResult>,
    ),
    ApiError,
> {
    let result = state
        .ledger
        .reverse_transaction(ReverseTransaction {
            user_id,
            journal_entry_id: JournalEntryId::new(id),
            reason: request.reason,
            idempotency_key: idempotency_key(&headers)?,
            correlation_id: crate::api::request_correlation_id(),
            causation_id: None,
            occurred_at: request.occurred_at.unwrap_or_else(Utc::now),
        })
        .await
        .map_err(map_ledger_error)?;
    Ok((StatusCode::CREATED, Json(result)))
}

pub(crate) async fn replace_transaction(
    AuthenticatedUser(user_id): AuthenticatedUser,
    State(state): State<LedgerApiState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    ApiJson(request): ApiJson<ReplaceRequest>,
) -> Result<
    (
        StatusCode,
        Json<crate::contexts::ledger::public::ReplacementResult>,
    ),
    ApiError,
> {
    let amount = money(&state, &request.amount).await?;
    let tags = NormalizedTags::new(request.tags).map_err(map_ledger_error)?;
    let result = state
        .ledger
        .replace_transaction(ReplaceTransaction {
            user_id,
            original_journal_entry_id: JournalEntryId::new(id),
            account_id: LedgerAccountId::new(request.account_id),
            kind: request.kind,
            amount,
            description: request.description,
            category_id: request.category_id.map(CategoryId::new),
            note: request.note,
            tags,
            budget_visibility: request.budget_visibility,
            idempotency_key: idempotency_key(&headers)?,
            correlation_id: crate::api::request_correlation_id(),
            causation_id: None,
            occurred_at: request.occurred_at.unwrap_or_else(Utc::now),
        })
        .await
        .map_err(map_ledger_error)?;
    Ok((StatusCode::CREATED, Json(result)))
}

pub(crate) async fn transfer(
    AuthenticatedUser(user_id): AuthenticatedUser,
    State(state): State<LedgerApiState>,
    headers: HeaderMap,
    ApiJson(request): ApiJson<TransferRequest>,
) -> Result<
    (
        StatusCode,
        Json<crate::contexts::ledger::public::TransferResult>,
    ),
    ApiError,
> {
    let source_amount = money(&state, &request.source_amount).await?;
    let target_amount = money(&state, &request.target_amount).await?;
    let fee = match request.fee {
        Some(value) => Some(TransferFee {
            amount: money(&state, &value).await?,
        }),
        None => None,
    };
    let implied_rate = request
        .implied_rate
        .map(|value| value.parse::<Decimal>())
        .transpose()
        .map_err(|_| ApiError::bad_request("invalid decimal string"))?
        .map(|value| value.normalize());
    let result = state
        .ledger
        .transfer(TransferFunds {
            user_id,
            source_account_id: LedgerAccountId::new(request.source_account_id),
            target_account_id: LedgerAccountId::new(request.target_account_id),
            source_amount,
            target_amount,
            fee,
            implied_rate,
            description: request.description,
            idempotency_key: idempotency_key(&headers)?,
            correlation_id: crate::api::request_correlation_id(),
            causation_id: None,
            occurred_at: request.occurred_at.unwrap_or_else(Utc::now),
        })
        .await
        .map_err(map_ledger_error)?;
    Ok((StatusCode::CREATED, Json(result)))
}

pub(crate) async fn correct_balance(
    AuthenticatedUser(user_id): AuthenticatedUser,
    State(state): State<LedgerApiState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    ApiJson(request): ApiJson<BalanceCorrectionRequest>,
) -> Result<
    (
        StatusCode,
        Json<crate::contexts::ledger::public::FinancialChangeResult>,
    ),
    ApiError,
> {
    let target = money(&state, &request.target_display_balance).await?;
    let result = state
        .ledger
        .correct_balance(CorrectBalance {
            user_id,
            account_id: LedgerAccountId::new(id),
            target_display_balance: target,
            expected_balance_version: request.expected_balance_version,
            reason: request.reason,
            observed_at: request.observed_at,
            idempotency_key: idempotency_key(&headers)?,
            correlation_id: crate::api::request_correlation_id(),
            causation_id: None,
            occurred_at: request.occurred_at.unwrap_or_else(Utc::now),
        })
        .await
        .map_err(map_ledger_error)?;
    Ok((StatusCode::CREATED, Json(result)))
}

pub(crate) async fn list_reconciliations(
    AuthenticatedUser(user_id): AuthenticatedUser,
    State(state): State<LedgerApiState>,
) -> Result<Json<Vec<crate::contexts::ledger::public::ReconciliationView>>, ApiError> {
    state
        .ledger
        .list_reconciliations(user_id)
        .await
        .map(Json)
        .map_err(map_ledger_error)
}

pub(crate) async fn get_reconciliation(
    AuthenticatedUser(user_id): AuthenticatedUser,
    State(state): State<LedgerApiState>,
    Path(id): Path<Uuid>,
) -> Result<Json<crate::contexts::ledger::public::ReconciliationView>, ApiError> {
    state
        .ledger
        .get_reconciliation(user_id, ReconciliationCaseId::new(id))
        .await
        .map(Json)
        .map_err(map_ledger_error)
}

pub(crate) async fn approve_reconciliation(
    AuthenticatedUser(user_id): AuthenticatedUser,
    State(state): State<LedgerApiState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    ApiJson(request): ApiJson<ApproveReconciliationRequest>,
) -> Result<Json<crate::contexts::ledger::public::ReconciliationResult>, ApiError> {
    state
        .ledger
        .approve_reconciliation(ApproveReconciliation {
            user_id,
            case_id: ReconciliationCaseId::new(id),
            expected_version: ReconciliationVersion::new(request.expected_version)
                .map_err(map_ledger_error)?,
            expected_balance_version: BalanceVersion::new(request.expected_balance_version)
                .map_err(map_ledger_error)?,
            reason: request.reason,
            idempotency_key: idempotency_key(&headers)?,
            correlation_id: crate::api::request_correlation_id(),
            causation_id: None,
            occurred_at: request.occurred_at.unwrap_or_else(Utc::now),
        })
        .await
        .map(Json)
        .map_err(map_ledger_error)
}

pub(crate) async fn dismiss_reconciliation(
    AuthenticatedUser(user_id): AuthenticatedUser,
    State(state): State<LedgerApiState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    ApiJson(request): ApiJson<DismissReconciliationRequest>,
) -> Result<Json<crate::contexts::ledger::public::ReconciliationResult>, ApiError> {
    state
        .ledger
        .dismiss_reconciliation(DismissReconciliation {
            user_id,
            case_id: ReconciliationCaseId::new(id),
            expected_version: ReconciliationVersion::new(request.expected_version)
                .map_err(map_ledger_error)?,
            reason: request.reason,
            idempotency_key: idempotency_key(&headers)?,
            correlation_id: crate::api::request_correlation_id(),
            occurred_at: request.occurred_at.unwrap_or_else(Utc::now),
        })
        .await
        .map(Json)
        .map_err(map_ledger_error)
}

fn idempotency_key(headers: &HeaderMap) -> Result<IdempotencyKey, ApiError> {
    let value = headers
        .get("Idempotency-Key")
        .ok_or_else(|| ApiError::bad_request("missing Idempotency-Key header"))?
        .to_str()
        .map_err(|_| ApiError::bad_request("invalid Idempotency-Key header"))?;
    IdempotencyKey::new(value).map_err(|_| ApiError::bad_request("invalid Idempotency-Key header"))
}

fn currency(value: &str) -> Result<CurrencyCode, ApiError> {
    CurrencyCode::new(value).map_err(|_| ApiError::bad_request("invalid currency code"))
}

async fn money(state: &LedgerApiState, request: &MoneyRequest) -> Result<Money, ApiError> {
    let code = currency(&request.currency)?;
    money_for(state, request.amount.clone(), code).await
}

async fn money_for(
    state: &LedgerApiState,
    amount: String,
    code: CurrencyCode,
) -> Result<Money, ApiError> {
    let definition = state
        .currencies
        .require_enabled(code.clone())
        .await
        .map_err(map_currency_error)?;
    let raw = amount
        .parse::<Decimal>()
        .map_err(|_| ApiError::bad_request("invalid decimal string"))?;
    Money::new(raw, code.clone(), u32::from(definition.minor_unit))
        .map_err(|_| ApiError::bad_request("invalid money amount"))?;
    Money::new(raw.normalize(), code, u32::from(definition.minor_unit))
        .map_err(|_| ApiError::bad_request("invalid money amount"))
}

fn account_version(value: i64) -> Result<AccountVersion, ApiError> {
    AccountVersion::new(value).map_err(map_ledger_error)
}

fn cursor(query: &ActivityQuery) -> Result<Option<ActivityCursor>, ApiError> {
    cursor_values(query.after_occurred_at, query.after_sequence)
}

fn cursor_values(
    after_occurred_at: Option<chrono::DateTime<chrono::Utc>>,
    after_sequence: Option<i64>,
) -> Result<Option<ActivityCursor>, ApiError> {
    match (after_occurred_at, after_sequence) {
        (None, None) => Ok(None),
        (Some(occurred_at), Some(ledger_sequence)) if ledger_sequence > 0 => {
            Ok(Some(ActivityCursor {
                occurred_at,
                ledger_sequence,
            }))
        }
        _ => Err(ApiError::bad_request(
            "cursor requires after_occurred_at and positive after_sequence",
        )),
    }
}

fn map_currency_error(error: CurrencyError) -> ApiError {
    if error.is_not_found() || error.is_disabled() {
        ApiError::bad_request("currency is unknown or inactive")
    } else {
        ApiError::internal(
            "reference_data.persistence",
            "currency storage operation failed",
        )
    }
}

fn map_ledger_error(error: LedgerError) -> ApiError {
    if error.is_not_found() || error.is_tenant_mismatch() {
        ApiError::not_found("ledger resource was not found")
    } else if error.is_version_conflict()
        || error.is_idempotency_conflict()
        || error.is_account_archived()
        || error.is_stale_observed_balance()
    {
        ApiError::conflict("ledger conflict")
    } else if error.is_persistence() {
        ApiError::internal("ledger.persistence", "ledger storage operation failed")
    } else {
        ApiError::bad_request("invalid ledger request")
    }
}
