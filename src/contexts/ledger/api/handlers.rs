use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use chrono::Utc;
use rust_decimal::Decimal;
use uuid::Uuid;

use crate::api::v2::{AuthenticatedUser, V2ApiError, V2Json};
use crate::api::v2_state::LedgerApiState;
use crate::contexts::classification::public::CategoryId;
use crate::contexts::ledger::public::{
    AccountVersion, ActivityCursor, AnnotationChanges, AnnotationVersion, ApproveReconciliation,
    ArchiveAccount, BalanceVersion, CategoryReference, CorrectBalance, DismissReconciliation,
    JournalEntryId, LedgerAccountId, LedgerError, NormalizedTags, OpenAccount,
    ReconciliationCaseId, ReconciliationVersion, RecordManualTransaction, RenameAccount,
    ReplaceTransaction, RestoreAccount, ReverseTransaction, TransferFee, TransferFunds,
    UpdateTransactionAnnotation,
};
use crate::contexts::reference_data::public::{CurrencyCatalog, CurrencyError};
use crate::shared_kernel::{CorrelationId, CurrencyCode, IdempotencyKey, Money};

use super::dto::{
    ActivityQuery, AnnotationRequest, ApproveReconciliationRequest, BalanceCorrectionRequest,
    DismissReconciliationRequest, ExpectedAccountVersionRequest, MoneyRequest, OpenAccountRequest,
    RecordTransactionRequest, RenameAccountRequest, ReplaceRequest, ReverseRequest,
    TransferRequest,
};

pub(crate) async fn open_account(
    AuthenticatedUser(user_id): AuthenticatedUser,
    State(state): State<LedgerApiState>,
    headers: HeaderMap,
    V2Json(request): V2Json<OpenAccountRequest>,
) -> Result<
    (
        StatusCode,
        Json<crate::contexts::ledger::public::AccountResult>,
    ),
    V2ApiError,
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
            correlation_id: CorrelationId::generate(),
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
) -> Result<Json<Vec<crate::contexts::ledger::public::AccountView>>, V2ApiError> {
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
) -> Result<Json<crate::contexts::ledger::public::AccountView>, V2ApiError> {
    state
        .ledger
        .get_account(user_id, LedgerAccountId::new(id))
        .await
        .map(Json)
        .map_err(map_ledger_error)
}

pub(crate) async fn rename_account(
    AuthenticatedUser(user_id): AuthenticatedUser,
    State(state): State<LedgerApiState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    V2Json(request): V2Json<RenameAccountRequest>,
) -> Result<Json<crate::contexts::ledger::public::AccountResult>, V2ApiError> {
    let command = RenameAccount {
        user_id,
        account_id: LedgerAccountId::new(id),
        name: request.name,
        expected_version: account_version(request.expected_version)?,
        idempotency_key: idempotency_key(&headers)?,
        correlation_id: CorrelationId::generate(),
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
    V2Json(request): V2Json<ExpectedAccountVersionRequest>,
) -> Result<Json<crate::contexts::ledger::public::AccountResult>, V2ApiError> {
    state
        .ledger
        .archive_account(ArchiveAccount {
            user_id,
            account_id: LedgerAccountId::new(id),
            expected_version: account_version(request.expected_version)?,
            idempotency_key: idempotency_key(&headers)?,
            correlation_id: CorrelationId::generate(),
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
    V2Json(request): V2Json<ExpectedAccountVersionRequest>,
) -> Result<Json<crate::contexts::ledger::public::AccountResult>, V2ApiError> {
    state
        .ledger
        .restore_account(RestoreAccount {
            user_id,
            account_id: LedgerAccountId::new(id),
            expected_version: account_version(request.expected_version)?,
            idempotency_key: idempotency_key(&headers)?,
            correlation_id: CorrelationId::generate(),
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
) -> Result<Json<Vec<crate::contexts::ledger::public::JournalView>>, V2ApiError> {
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
    V2Json(request): V2Json<RecordTransactionRequest>,
) -> Result<
    (
        StatusCode,
        Json<crate::contexts::ledger::public::TransactionResult>,
    ),
    V2ApiError,
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
            correlation_id: CorrelationId::generate(),
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
    Query(query): Query<ActivityQuery>,
) -> Result<Json<Vec<crate::contexts::ledger::public::JournalView>>, V2ApiError> {
    state
        .ledger
        .list_journals(user_id, cursor(&query)?, query.limit.unwrap_or(50))
        .await
        .map(Json)
        .map_err(map_ledger_error)
}

pub(crate) async fn get_transaction(
    AuthenticatedUser(user_id): AuthenticatedUser,
    State(state): State<LedgerApiState>,
    Path(id): Path<Uuid>,
) -> Result<Json<crate::contexts::ledger::public::JournalView>, V2ApiError> {
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
    V2Json(request): V2Json<AnnotationRequest>,
) -> Result<Json<crate::contexts::ledger::public::AnnotationResult>, V2ApiError> {
    if request.clear_category && request.category_id.is_some()
        || request.clear_note && request.note.is_some()
    {
        return Err(V2ApiError::bad_request(
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
            correlation_id: CorrelationId::generate(),
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
    V2Json(request): V2Json<ReverseRequest>,
) -> Result<
    (
        StatusCode,
        Json<crate::contexts::ledger::public::FinancialChangeResult>,
    ),
    V2ApiError,
> {
    let result = state
        .ledger
        .reverse_transaction(ReverseTransaction {
            user_id,
            journal_entry_id: JournalEntryId::new(id),
            reason: request.reason,
            idempotency_key: idempotency_key(&headers)?,
            correlation_id: CorrelationId::generate(),
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
    V2Json(request): V2Json<ReplaceRequest>,
) -> Result<
    (
        StatusCode,
        Json<crate::contexts::ledger::public::ReplacementResult>,
    ),
    V2ApiError,
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
            correlation_id: CorrelationId::generate(),
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
    V2Json(request): V2Json<TransferRequest>,
) -> Result<
    (
        StatusCode,
        Json<crate::contexts::ledger::public::TransferResult>,
    ),
    V2ApiError,
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
        .map_err(|_| V2ApiError::bad_request("invalid decimal string"))?
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
            correlation_id: CorrelationId::generate(),
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
    V2Json(request): V2Json<BalanceCorrectionRequest>,
) -> Result<
    (
        StatusCode,
        Json<crate::contexts::ledger::public::FinancialChangeResult>,
    ),
    V2ApiError,
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
            correlation_id: CorrelationId::generate(),
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
) -> Result<Json<Vec<crate::contexts::ledger::public::ReconciliationView>>, V2ApiError> {
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
) -> Result<Json<crate::contexts::ledger::public::ReconciliationView>, V2ApiError> {
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
    V2Json(request): V2Json<ApproveReconciliationRequest>,
) -> Result<Json<crate::contexts::ledger::public::ReconciliationResult>, V2ApiError> {
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
            correlation_id: CorrelationId::generate(),
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
    V2Json(request): V2Json<DismissReconciliationRequest>,
) -> Result<Json<crate::contexts::ledger::public::ReconciliationResult>, V2ApiError> {
    state
        .ledger
        .dismiss_reconciliation(DismissReconciliation {
            user_id,
            case_id: ReconciliationCaseId::new(id),
            expected_version: ReconciliationVersion::new(request.expected_version)
                .map_err(map_ledger_error)?,
            reason: request.reason,
            idempotency_key: idempotency_key(&headers)?,
            correlation_id: CorrelationId::generate(),
            occurred_at: request.occurred_at.unwrap_or_else(Utc::now),
        })
        .await
        .map(Json)
        .map_err(map_ledger_error)
}

fn idempotency_key(headers: &HeaderMap) -> Result<IdempotencyKey, V2ApiError> {
    let value = headers
        .get("Idempotency-Key")
        .ok_or_else(|| V2ApiError::bad_request("missing Idempotency-Key header"))?
        .to_str()
        .map_err(|_| V2ApiError::bad_request("invalid Idempotency-Key header"))?;
    IdempotencyKey::new(value)
        .map_err(|_| V2ApiError::bad_request("invalid Idempotency-Key header"))
}

fn currency(value: &str) -> Result<CurrencyCode, V2ApiError> {
    CurrencyCode::new(value).map_err(|_| V2ApiError::bad_request("invalid currency code"))
}

async fn money(state: &LedgerApiState, request: &MoneyRequest) -> Result<Money, V2ApiError> {
    let code = currency(&request.currency)?;
    money_for(state, request.amount.clone(), code).await
}

async fn money_for(
    state: &LedgerApiState,
    amount: String,
    code: CurrencyCode,
) -> Result<Money, V2ApiError> {
    let definition = state
        .currencies
        .require_enabled(code.clone())
        .await
        .map_err(map_currency_error)?;
    let raw = amount
        .parse::<Decimal>()
        .map_err(|_| V2ApiError::bad_request("invalid decimal string"))?;
    Money::new(raw, code.clone(), u32::from(definition.minor_unit))
        .map_err(|_| V2ApiError::bad_request("invalid money amount"))?;
    Money::new(raw.normalize(), code, u32::from(definition.minor_unit))
        .map_err(|_| V2ApiError::bad_request("invalid money amount"))
}

fn account_version(value: i64) -> Result<AccountVersion, V2ApiError> {
    AccountVersion::new(value).map_err(map_ledger_error)
}

fn cursor(query: &ActivityQuery) -> Result<Option<ActivityCursor>, V2ApiError> {
    match (query.after_occurred_at, query.after_sequence) {
        (None, None) => Ok(None),
        (Some(occurred_at), Some(ledger_sequence)) if ledger_sequence > 0 => {
            Ok(Some(ActivityCursor {
                occurred_at,
                ledger_sequence,
            }))
        }
        _ => Err(V2ApiError::bad_request(
            "cursor requires after_occurred_at and positive after_sequence",
        )),
    }
}

fn map_currency_error(error: CurrencyError) -> V2ApiError {
    if error.is_not_found() || error.is_disabled() {
        V2ApiError::bad_request("currency is unknown or inactive")
    } else {
        V2ApiError::internal()
    }
}

fn map_ledger_error(error: LedgerError) -> V2ApiError {
    if error.is_not_found() || error.is_tenant_mismatch() {
        V2ApiError::not_found("ledger resource was not found")
    } else if error.is_version_conflict()
        || error.is_idempotency_conflict()
        || error.is_account_archived()
        || error.is_stale_observed_balance()
    {
        V2ApiError::conflict("ledger conflict")
    } else if error.is_persistence() {
        V2ApiError::internal()
    } else {
        V2ApiError::bad_request("invalid ledger request")
    }
}
