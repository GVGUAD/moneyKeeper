//! Portfolio-owned durable adapter for the public Ledger cash coordinator.
use crate::contexts::ledger::public::*;
use crate::contexts::portfolio::public::*;
use crate::integration::process_managers::portfolio_cash_settlement::{
    PortfolioCashProcessState, PortfolioCashSettlementCoordinator, PortfolioCashSettlementProcess,
};
use crate::shared_kernel::{CorrelationId, CurrencyCode, IdempotencyKey, UserId};
use chrono::Utc;
use sqlx::Row;

#[derive(Clone)]
pub struct PortfolioCashSettlementWorker {
    pool: sqlx::PgPool,
    ledger: LedgerFacade,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PortfolioCashWorkerReport {
    pub claimed: bool,
    pub posted: bool,
    pub retry_due: bool,
}
impl PortfolioCashSettlementWorker {
    pub fn new(pool: sqlx::PgPool, ledger: LedgerFacade) -> Self {
        Self { pool, ledger }
    }
    pub async fn run_once(&self) -> anyhow::Result<PortfolioCashWorkerReport> {
        let mut tx = self.pool.begin().await?;
        let row=sqlx::query("SELECT transaction_id,user_id,cash_flow,cash_account_id,amount,currency,correlation_id,action,ledger_journal_id,ledger_reversal_id FROM portfolio.cash_settlement_processes WHERE state IN ('pending','retrying') ORDER BY updated_at,transaction_id FOR UPDATE SKIP LOCKED LIMIT 1").fetch_optional(&mut *tx).await?;
        let Some(row) = row else {
            tx.rollback().await?;
            return Ok(Default::default());
        };
        let transaction_id = PortfolioTransactionId::new(row.get("transaction_id"));
        let user_id = UserId::new(row.get("user_id"));
        let currency = CurrencyCode::new(row.get::<String, _>("currency"))?;
        let correlation = CorrelationId::new(row.get("correlation_id"));
        sqlx::query("UPDATE portfolio.cash_settlement_processes SET state='retrying',attempt_count=attempt_count+1,updated_at=clock_timestamp() WHERE transaction_id=$1 AND user_id=$2").bind(transaction_id.into_uuid()).bind(user_id.into_uuid()).execute(&mut *tx).await?;
        tx.commit().await?;
        let control = self
            .ledger
            .ensure_typed_control_account(EnsureTypedControlAccount {
                metadata: InternalCommandMetadata {
                    user_id,
                    source: SourceReference::new(
                        "portfolio",
                        format!("transaction:{transaction_id}"),
                        "cash-control",
                    )?,
                    correlation_id: correlation,
                    causation_id: None,
                    idempotency_key: IdempotencyKey::new(format!("portfolio-control:{currency}"))?,
                    occurred_at: Utc::now(),
                },
                role: ControlAccountRole::PortfolioCashClearing,
                subject_reference: "portfolio".into(),
                currency: currency.clone(),
            })
            .await?;
        let mut process = PortfolioCashSettlementProcess {
            transaction_id,
            user_id,
            cash_account_id: LedgerAccountId::new(row.get("cash_account_id")),
            control_account_id: control.account_id,
            amount: row.get("amount"),
            currency,
            cash_flow: if row.get::<String, _>("cash_flow") == "outgoing" {
                CashFlowDirection::Outgoing
            } else {
                CashFlowDirection::Incoming
            },
            state: PortfolioCashProcessState::Retrying,
            correlation_id: correlation,
            journal_id: row
                .get::<Option<uuid::Uuid>, _>("ledger_journal_id")
                .map(JournalEntryId::new),
            reversal_journal_id: row
                .get::<Option<uuid::Uuid>, _>("ledger_reversal_id")
                .map(JournalEntryId::new),
            last_error: None,
        };
        let coordinator = PortfolioCashSettlementCoordinator::new(self.ledger.clone());
        let outcome = if row.get::<String, _>("action") == "post" {
            coordinator.post(&mut process, Utc::now()).await
        } else {
            coordinator
                .cancel_or_reverse(
                    &mut process,
                    "Portfolio transaction reversed".into(),
                    Utc::now(),
                )
                .await
        };
        let (state, journal, reversal, error) = match outcome {
            Ok(()) => (
                state_db(process.state),
                process.journal_id.map(JournalEntryId::into_uuid),
                process.reversal_journal_id.map(JournalEntryId::into_uuid),
                None,
            ),
            Err(e) => ("retrying", None, None, Some(e)),
        };
        let mut final_tx = self.pool.begin().await?;
        sqlx::query("UPDATE portfolio.cash_settlement_processes SET state=$3,ledger_journal_id=COALESCE($4,ledger_journal_id),ledger_reversal_id=COALESCE($5,ledger_reversal_id),last_error=$6,updated_at=clock_timestamp(),completed_at=CASE WHEN $3 IN ('posted','failed','cancelled_no_financial_effect','reversed') THEN clock_timestamp() ELSE NULL END WHERE transaction_id=$1 AND user_id=$2").bind(transaction_id.into_uuid()).bind(user_id.into_uuid()).bind(state).bind(journal).bind(reversal).bind(error).execute(&mut *final_tx).await?;
        let event=match state{
            "posted"=>journal.map(|id|(CASH_SETTLEMENT_POSTED_V1,serde_json::json!({"transaction_id":transaction_id,"journal_id":id}))),
            "reversed"=>process.journal_id.zip(reversal.map(JournalEntryId::new)).map(|(original,reversal)|(CASH_SETTLEMENT_REVERSED_V1,serde_json::json!({"transaction_id":transaction_id,"journal_id":original,"reversal_journal_id":reversal}))),
            "cancelled_no_financial_effect"=>Some((CASH_SETTLEMENT_CANCELLED_V1,serde_json::json!({"transaction_id":transaction_id}))),
            _=>None,
        };
        if let Some((event_type, payload)) = event {
            sqlx::query("INSERT INTO integration.outbox_messages(message_id,event_id,message_schema_version,context_name,aggregate_id,aggregate_version,event_type,user_id,occurred_at,correlation_id,payload) VALUES($1,$2,1,'portfolio',$3,1,$4,$5,$6,$7,$8)").bind(uuid::Uuid::new_v4()).bind(uuid::Uuid::new_v4()).bind(transaction_id.to_string()).bind(event_type).bind(user_id.into_uuid()).bind(Utc::now()).bind(correlation.into_uuid()).bind(payload).execute(&mut *final_tx).await?;
        }
        final_tx.commit().await?;
        Ok(PortfolioCashWorkerReport {
            claimed: true,
            posted: state == "posted",
            retry_due: state == "retrying",
        })
    }
}
fn state_db(v: PortfolioCashProcessState) -> &'static str {
    match v {
        PortfolioCashProcessState::Pending => "pending",
        PortfolioCashProcessState::Posted => "posted",
        PortfolioCashProcessState::Retrying => "retrying",
        PortfolioCashProcessState::Failed => "failed",
        PortfolioCashProcessState::CancelledNoFinancialEffect => "cancelled_no_financial_effect",
        PortfolioCashProcessState::Reversed => "reversed",
    }
}
