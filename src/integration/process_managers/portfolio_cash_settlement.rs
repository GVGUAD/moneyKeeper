//! Durable, race-safe Portfolio-to-Ledger cash settlement coordinator.

use crate::{
    contexts::{
        ledger::public::*,
        portfolio::{application::ports::PortfolioLedger, public::*},
    },
    shared_kernel::{CorrelationId, CurrencyCode, IdempotencyKey, Money, UserId},
};
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PortfolioCashProcessState {
    Pending,
    Posted,
    Retrying,
    Failed,
    CancelledNoFinancialEffect,
    Reversed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PortfolioCashSettlementProcess {
    pub transaction_id: PortfolioTransactionId,
    pub user_id: UserId,
    pub cash_account_id: LedgerAccountId,
    pub control_account_id: LedgerAccountId,
    pub amount: Decimal,
    pub currency: CurrencyCode,
    pub cash_flow: CashFlowDirection,
    pub state: PortfolioCashProcessState,
    pub correlation_id: CorrelationId,
    pub journal_id: Option<JournalEntryId>,
    pub reversal_journal_id: Option<JournalEntryId>,
    pub last_error: Option<String>,
}

impl PortfolioCashSettlementProcess {
    pub fn source_operation_id(&self) -> String {
        format!("portfolio-cash:v1:{}", self.transaction_id)
    }
    pub fn posting_key(&self) -> Result<IdempotencyKey, crate::shared_kernel::IdempotencyKeyError> {
        IdempotencyKey::new(self.source_operation_id())
    }
    pub fn reversal_key(
        &self,
    ) -> Result<IdempotencyKey, crate::shared_kernel::IdempotencyKeyError> {
        IdempotencyKey::new(format!("portfolio-cash:v1:reverse:{}", self.transaction_id))
    }
}

#[derive(Clone)]
pub struct PortfolioCashSettlementCoordinator<L> {
    ledger: L,
}
impl<L: PortfolioLedger> PortfolioCashSettlementCoordinator<L> {
    pub fn new(ledger: L) -> Self {
        Self { ledger }
    }
    pub async fn post(
        &self,
        process: &mut PortfolioCashSettlementProcess,
        now: DateTime<Utc>,
    ) -> Result<(), String> {
        if matches!(
            process.state,
            PortfolioCashProcessState::Posted
                | PortfolioCashProcessState::CancelledNoFinancialEffect
                | PortfolioCashProcessState::Reversed
        ) {
            return Ok(());
        }
        process.state = PortfolioCashProcessState::Retrying;
        let result = self
            .ledger
            .record_cash_control_settlement(RecordCashControlSettlement {
                metadata: metadata(
                    process,
                    process.posting_key().map_err(|e| e.to_string())?,
                    now,
                    "post",
                )?,
                cash_account_id: process.cash_account_id,
                control_account_id: process.control_account_id,
                amount: Money::new(process.amount, process.currency.clone(), 8)
                    .map_err(|e| e.to_string())?,
                cash_flow: process.cash_flow,
                source_operation_id: process.source_operation_id(),
            })
            .await
            .map_err(|e| {
                process.last_error = Some(e.to_string());
                e.to_string()
            })?;
        if result.cancelled {
            process.state = PortfolioCashProcessState::CancelledNoFinancialEffect;
            process.journal_id = None
        } else {
            process.state = PortfolioCashProcessState::Posted;
            process.journal_id = result.journal_entry_id
        }
        process.last_error = None;
        Ok(())
    }
    pub async fn cancel_or_reverse(
        &self,
        process: &mut PortfolioCashSettlementProcess,
        reason: String,
        now: DateTime<Utc>,
    ) -> Result<(), String> {
        if matches!(
            process.state,
            PortfolioCashProcessState::CancelledNoFinancialEffect
                | PortfolioCashProcessState::Reversed
        ) {
            return Ok(());
        }
        let result = self
            .ledger
            .cancel_or_reverse_cash_control_settlement(CancelOrReverseCashControlSettlement {
                metadata: metadata(
                    process,
                    process.reversal_key().map_err(|e| e.to_string())?,
                    now,
                    "reverse",
                )?,
                source_operation_id: process.source_operation_id(),
                reason,
            })
            .await
            .map_err(|e| {
                process.last_error = Some(e.to_string());
                e.to_string()
            })?;
        if result.cancelled && result.journal_entry_id.is_none() {
            process.state = PortfolioCashProcessState::CancelledNoFinancialEffect;
            process.journal_id = None;
            process.reversal_journal_id = None
        } else {
            process.state = PortfolioCashProcessState::Reversed;
            process.reversal_journal_id = result.journal_entry_id
        }
        process.last_error = None;
        Ok(())
    }
}
fn metadata(
    process: &PortfolioCashSettlementProcess,
    key: IdempotencyKey,
    now: DateTime<Utc>,
    item: &str,
) -> Result<InternalCommandMetadata, String> {
    Ok(InternalCommandMetadata {
        user_id: process.user_id,
        source: SourceReference::new(
            "portfolio",
            format!("transaction:{}", process.transaction_id),
            format!("cash-{item}"),
        )
        .map_err(|e| e.to_string())?,
        correlation_id: process.correlation_id,
        causation_id: None,
        idempotency_key: key,
        occurred_at: now,
    })
}
