use axum::Json;
use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use chrono::NaiveDate;
use uuid::Uuid;

use crate::api::dto::{
    AccountDetailsDto, AccountResponse, BalanceResponse, CreateAccountRequest, UpdateAccountRequest,
};
use crate::api::error::AppError;
use crate::api::middleware::AuthUser;
use crate::api::state::AppState;
use crate::domain::account::{
    Account, AccountDetails, AccountType, BinanceDetails, CompoundingPeriod, InvestmentDetails,
    LoanDetails, LoanDirection, SavingsDetails,
};
use crate::domain::error::DomainError;

fn dto_to_details(
    dto: Option<AccountDetailsDto>,
    account_type: &AccountType,
) -> anyhow::Result<AccountDetails> {
    match (dto, account_type) {
        (
            Some(AccountDetailsDto::Savings {
                interest_rate,
                compounding_period,
            }),
            AccountType::Savings,
        ) => Ok(AccountDetails::Savings(SavingsDetails {
            account_id: Uuid::nil(),
            interest_rate,
            compounding_period: CompoundingPeriod::from_str(&compounding_period)?,
        })),
        (
            Some(AccountDetailsDto::Loan {
                counterparty,
                direction,
                interest_rate,
                due_date,
            }),
            AccountType::Loan,
        ) => Ok(AccountDetails::Loan(LoanDetails {
            account_id: Uuid::nil(),
            counterparty,
            direction: LoanDirection::from_str(&direction)?,
            interest_rate,
            due_date: due_date.map(|d| d.parse::<NaiveDate>()).transpose()?,
        })),
        (Some(AccountDetailsDto::Investment { broker }), AccountType::Investment) => {
            Ok(AccountDetails::Investment(InvestmentDetails {
                account_id: Uuid::nil(),
                broker,
            }))
        }
        (Some(AccountDetailsDto::Binance { label }), AccountType::Binance) => {
            Ok(AccountDetails::Binance(BinanceDetails {
                account_id: Uuid::nil(),
                label,
            }))
        }
        (None, AccountType::Cash) | (None, AccountType::Bank) => Ok(AccountDetails::None),
        _ => {
            Err(DomainError::InvalidInput("mismatched account type and details".to_string()).into())
        }
    }
}

fn details_to_dto(details: &AccountDetails) -> Option<AccountDetailsDto> {
    match details {
        AccountDetails::Savings(s) => Some(AccountDetailsDto::Savings {
            interest_rate: s.interest_rate,
            compounding_period: s.compounding_period.as_str().to_string(),
        }),
        AccountDetails::Loan(l) => Some(AccountDetailsDto::Loan {
            counterparty: l.counterparty.clone(),
            direction: l.direction.as_str().to_string(),
            interest_rate: l.interest_rate,
            due_date: l.due_date.map(|d| d.to_string()),
        }),
        AccountDetails::Investment(i) => Some(AccountDetailsDto::Investment {
            broker: i.broker.clone(),
        }),
        AccountDetails::Binance(b) => Some(AccountDetailsDto::Binance {
            label: b.label.clone(),
        }),
        AccountDetails::None => None,
    }
}

fn account_to_response(a: Account, d: AccountDetails) -> AccountResponse {
    AccountResponse {
        id: a.id,
        name: a.name,
        account_type: a.account_type.as_str().to_string(),
        currency: a.currency,
        details: details_to_dto(&d),
        created_at: a.created_at,
        updated_at: a.updated_at,
    }
}

pub async fn create_account(
    State(state): State<AppState>,
    Extension(AuthUser(user_id)): Extension<AuthUser>,
    Json(req): Json<CreateAccountRequest>,
) -> Result<(StatusCode, Json<AccountResponse>), AppError> {
    let account_type = AccountType::from_str(&req.account_type).map_err(|_| {
        DomainError::InvalidInput(format!("unknown account type: {}", req.account_type))
    })?;
    let details = dto_to_details(req.details, &account_type)?;
    let account = state
        .accounts
        .create(
            user_id,
            req.name,
            account_type,
            req.currency,
            details.clone(),
        )
        .await?;
    Ok((
        StatusCode::CREATED,
        Json(account_to_response(account, details)),
    ))
}

pub async fn list_accounts(
    State(state): State<AppState>,
    Extension(AuthUser(user_id)): Extension<AuthUser>,
) -> Result<Json<Vec<AccountResponse>>, AppError> {
    let accounts = state.accounts.list(user_id).await?;
    Ok(Json(
        accounts
            .into_iter()
            .map(|(a, d)| account_to_response(a, d))
            .collect(),
    ))
}

pub async fn get_account(
    State(state): State<AppState>,
    Extension(AuthUser(user_id)): Extension<AuthUser>,
    Path(id): Path<Uuid>,
) -> Result<Json<AccountResponse>, AppError> {
    let (a, d) = state.accounts.get(id, user_id).await?;
    Ok(Json(account_to_response(a, d)))
}

pub async fn update_account(
    State(state): State<AppState>,
    Extension(AuthUser(user_id)): Extension<AuthUser>,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdateAccountRequest>,
) -> Result<Json<AccountResponse>, AppError> {
    let (a, d) = state
        .accounts
        .update(id, user_id, req.name, req.currency, None)
        .await?;
    Ok(Json(account_to_response(a, d)))
}

pub async fn delete_account(
    State(state): State<AppState>,
    Extension(AuthUser(user_id)): Extension<AuthUser>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, AppError> {
    state.accounts.delete(id, user_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn get_balance(
    State(state): State<AppState>,
    Extension(AuthUser(user_id)): Extension<AuthUser>,
    Path(id): Path<Uuid>,
) -> Result<Json<BalanceResponse>, AppError> {
    let (a, _) = state.accounts.get(id, user_id).await?;
    let balance = state.accounts.get_balance(id, user_id).await?;
    Ok(Json(BalanceResponse {
        account_id: id,
        balance,
        currency: a.currency,
    }))
}
