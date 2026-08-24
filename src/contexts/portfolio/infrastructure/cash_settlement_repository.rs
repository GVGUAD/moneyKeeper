//! PostgreSQL adapter for the Portfolio cash-settlement workflow port.

use async_trait::async_trait;
use chrono::Utc;
use sqlx::{PgPool, Row};

use super::super::application::ports::{
    CashSettlementAction, CashSettlementCompletion, CashSettlementServiceError,
    CashSettlementState, CashSettlementWork, PortfolioCashSettlementRepository,
};
use super::super::public::{
    CASH_SETTLEMENT_CANCELLED_V1, CASH_SETTLEMENT_POSTED_V1, CASH_SETTLEMENT_REVERSED_V1,
    PortfolioTransactionId,
};
use crate::contexts::ledger::public::{CashFlowDirection, JournalEntryId, LedgerAccountId};
use crate::shared_kernel::{CorrelationId, CurrencyCode, UserId};

#[derive(Clone)]
pub(crate) struct PgPortfolioCashSettlementRepository {
    pool: PgPool,
}

impl PgPortfolioCashSettlementRepository {
    pub(crate) fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl PortfolioCashSettlementRepository for PgPortfolioCashSettlementRepository {
    async fn claim_next(&self) -> Result<Option<CashSettlementWork>, CashSettlementServiceError> {
        let mut transaction = self.pool.begin().await.map_err(database)?;
        let row = sqlx::query(
            "SELECT transaction_id,user_id,cash_flow,cash_account_id,amount,currency,correlation_id,action,ledger_journal_id,ledger_reversal_id \
             FROM portfolio.cash_settlement_processes \
             WHERE state IN ('pending','retrying') \
             ORDER BY updated_at,transaction_id FOR UPDATE SKIP LOCKED LIMIT 1",
        )
        .fetch_optional(&mut *transaction)
        .await
        .map_err(database)?;
        let Some(row) = row else {
            transaction.rollback().await.map_err(database)?;
            return Ok(None);
        };
        let transaction_id = PortfolioTransactionId::new(row.get("transaction_id"));
        let user_id = UserId::new(row.get("user_id"));
        sqlx::query(
            "UPDATE portfolio.cash_settlement_processes \
             SET state='retrying',attempt_count=attempt_count+1,updated_at=clock_timestamp() \
             WHERE transaction_id=$1 AND user_id=$2",
        )
        .bind(transaction_id.into_uuid())
        .bind(user_id.into_uuid())
        .execute(&mut *transaction)
        .await
        .map_err(database)?;
        transaction.commit().await.map_err(database)?;

        Ok(Some(CashSettlementWork {
            transaction_id,
            user_id,
            cash_account_id: LedgerAccountId::new(row.get("cash_account_id")),
            amount: row.get("amount"),
            currency: CurrencyCode::new(row.get::<String, _>("currency"))
                .map_err(CashSettlementServiceError::invalid)?,
            cash_flow: if row.get::<String, _>("cash_flow") == "outgoing" {
                CashFlowDirection::Outgoing
            } else {
                CashFlowDirection::Incoming
            },
            correlation_id: CorrelationId::new(row.get("correlation_id")),
            action: if row.get::<String, _>("action") == "post" {
                CashSettlementAction::Post
            } else {
                CashSettlementAction::CancelOrReverse
            },
            journal_id: row
                .get::<Option<uuid::Uuid>, _>("ledger_journal_id")
                .map(JournalEntryId::new),
            reversal_journal_id: row
                .get::<Option<uuid::Uuid>, _>("ledger_reversal_id")
                .map(JournalEntryId::new),
        }))
    }

    async fn complete(
        &self,
        completion: CashSettlementCompletion,
    ) -> Result<(), CashSettlementServiceError> {
        let state = state_database(completion.state);
        let journal = completion.journal_id.map(JournalEntryId::into_uuid);
        let reversal = completion
            .reversal_journal_id
            .map(JournalEntryId::into_uuid);
        let now = Utc::now();
        let mut transaction = self.pool.begin().await.map_err(database)?;
        sqlx::query(
            "UPDATE portfolio.cash_settlement_processes \
             SET state=$3,ledger_journal_id=COALESCE($4,ledger_journal_id), \
                 ledger_reversal_id=COALESCE($5,ledger_reversal_id),last_error=$6, \
                 updated_at=clock_timestamp(), \
                 completed_at=CASE WHEN $3 IN ('posted','failed','cancelled_no_financial_effect','reversed') \
                                   THEN clock_timestamp() ELSE NULL END \
             WHERE transaction_id=$1 AND user_id=$2",
        )
        .bind(completion.work.transaction_id.into_uuid())
        .bind(completion.work.user_id.into_uuid())
        .bind(state)
        .bind(journal)
        .bind(reversal)
        .bind(completion.last_error)
        .execute(&mut *transaction)
        .await
        .map_err(database)?;

        let event = match state {
            "posted" => journal.map(|journal_id| {
                (
                    CASH_SETTLEMENT_POSTED_V1,
                    serde_json::json!({
                        "transaction_id": completion.work.transaction_id,
                        "journal_id": journal_id
                    }),
                )
            }),
            "reversed" => completion
                .journal_id
                .zip(completion.reversal_journal_id)
                .map(|(original, reversal)| {
                    (
                        CASH_SETTLEMENT_REVERSED_V1,
                        serde_json::json!({
                            "transaction_id": completion.work.transaction_id,
                            "journal_id": original,
                            "reversal_journal_id": reversal
                        }),
                    )
                }),
            "cancelled_no_financial_effect" => Some((
                CASH_SETTLEMENT_CANCELLED_V1,
                serde_json::json!({"transaction_id": completion.work.transaction_id}),
            )),
            _ => None,
        };
        if let Some((event_type, payload)) = event {
            sqlx::query(
                "INSERT INTO integration.outbox_messages \
                 (message_id,event_id,message_schema_version,context_name,aggregate_id,aggregate_version, \
                  event_type,user_id,occurred_at,correlation_id,payload) \
                 VALUES($1,$2,1,'portfolio',$3,1,$4,$5,$6,$7,$8)",
            )
            .bind(uuid::Uuid::new_v4())
            .bind(uuid::Uuid::new_v4())
            .bind(completion.work.transaction_id.to_string())
            .bind(event_type)
            .bind(completion.work.user_id.into_uuid())
            .bind(now)
            .bind(completion.work.correlation_id.into_uuid())
            .bind(payload)
            .execute(&mut *transaction)
            .await
            .map_err(database)?;
        }
        transaction.commit().await.map_err(database)
    }
}

fn state_database(state: CashSettlementState) -> &'static str {
    match state {
        CashSettlementState::Retrying => "retrying",
        CashSettlementState::Posted => "posted",
        CashSettlementState::Failed => "failed",
        CashSettlementState::CancelledNoFinancialEffect => "cancelled_no_financial_effect",
        CashSettlementState::Reversed => "reversed",
    }
}

fn database(error: sqlx::Error) -> CashSettlementServiceError {
    CashSettlementServiceError::persistence(error)
}
