//! Portfolio-owned orchestration for durable cash settlement through Ledger.

use std::error::Error as _;

use chrono::{DateTime, Utc};
use tracing::Instrument as _;

use super::ports::{
    CashSettlementAction, CashSettlementCompletion, CashSettlementServiceError,
    CashSettlementState, CashSettlementWork, PortfolioCashSettlementRepository, PortfolioLedger,
};
use crate::contexts::ledger::public::{
    CancelOrReverseCashControlSettlement, ControlAccountRole, EnsureTypedControlAccount,
    InternalCommandMetadata, JournalEntryId, LedgerAccountId, RecordCashControlSettlement,
    SourceReference,
};
use crate::shared_kernel::{IdempotencyKey, Money};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct PortfolioCashWorkerReport {
    pub claimed: bool,
    pub posted: bool,
    pub retry_due: bool,
}

#[derive(Clone)]
pub(crate) struct PortfolioCashSettlementService<R, L> {
    repository: R,
    ledger: L,
}

impl<R, L> PortfolioCashSettlementService<R, L>
where
    R: PortfolioCashSettlementRepository,
    L: PortfolioLedger,
{
    pub(crate) fn new(repository: R, ledger: L) -> Self {
        Self { repository, ledger }
    }

    pub(crate) async fn run_once(
        &self,
    ) -> Result<PortfolioCashWorkerReport, CashSettlementServiceError> {
        let Some(work) = self.repository.claim_next().await? else {
            return Ok(PortfolioCashWorkerReport::default());
        };
        let item_span = tracing::info_span!(
            "worker.item",
            operation = "portfolio.cash_settlement",
            transaction_id = %work.transaction_id,
            correlation_id = %work.correlation_id,
        );
        item_span.in_scope(|| {
            tracing::info!(
                event.name = "worker.item.claimed",
                outcome = "claimed",
                "Worker item claimed"
            );
        });
        let control = self
            .ledger
            .ensure_typed_control_account(EnsureTypedControlAccount {
                metadata: InternalCommandMetadata {
                    user_id: work.user_id,
                    source: SourceReference::new(
                        "portfolio",
                        format!("transaction:{}", work.transaction_id),
                        "cash-control",
                    )
                    .map_err(CashSettlementServiceError::invalid)?,
                    correlation_id: work.correlation_id,
                    causation_id: None,
                    idempotency_key: IdempotencyKey::new(format!(
                        "portfolio-control:{}",
                        work.currency
                    ))
                    .map_err(CashSettlementServiceError::invalid)?,
                    occurred_at: Utc::now(),
                },
                role: ControlAccountRole::PortfolioCashClearing,
                subject_reference: "portfolio".into(),
                currency: work.currency.clone(),
            })
            .instrument(item_span)
            .await
            .map_err(CashSettlementServiceError::ledger)?;

        let mut process = CashSettlementProcess::new(work, control.account_id);
        let coordinator = CashSettlementCoordinator::new(self.ledger.clone());
        let outcome = match process.work.action {
            CashSettlementAction::Post => coordinator.post(&mut process, Utc::now()).await,
            CashSettlementAction::CancelOrReverse => {
                coordinator
                    .cancel_or_reverse(
                        &mut process,
                        "Portfolio transaction reversed".into(),
                        Utc::now(),
                    )
                    .await
            }
        };
        let last_error = outcome.as_ref().err().map(|error| {
            error
                .source()
                .map(ToString::to_string)
                .unwrap_or_else(|| error.to_string())
        });
        if last_error.is_some() {
            process.state = CashSettlementState::Retrying;
        }
        self.repository
            .complete(CashSettlementCompletion {
                work: process.work,
                state: process.state,
                journal_id: process.journal_id,
                reversal_journal_id: process.reversal_journal_id,
                last_error,
            })
            .await?;
        Ok(PortfolioCashWorkerReport {
            claimed: true,
            posted: process.state == CashSettlementState::Posted,
            retry_due: process.state == CashSettlementState::Retrying,
        })
    }
}

struct CashSettlementProcess {
    work: CashSettlementWork,
    control_account_id: LedgerAccountId,
    state: CashSettlementState,
    journal_id: Option<JournalEntryId>,
    reversal_journal_id: Option<JournalEntryId>,
}

impl CashSettlementProcess {
    fn new(work: CashSettlementWork, control_account_id: LedgerAccountId) -> Self {
        Self {
            journal_id: work.journal_id,
            reversal_journal_id: work.reversal_journal_id,
            work,
            control_account_id,
            state: CashSettlementState::Retrying,
        }
    }

    fn source_operation_id(&self) -> String {
        format!("portfolio-cash:v1:{}", self.work.transaction_id)
    }

    fn posting_key(&self) -> Result<IdempotencyKey, CashSettlementServiceError> {
        IdempotencyKey::new(self.source_operation_id()).map_err(CashSettlementServiceError::invalid)
    }

    fn reversal_key(&self) -> Result<IdempotencyKey, CashSettlementServiceError> {
        IdempotencyKey::new(format!(
            "portfolio-cash:v1:reverse:{}",
            self.work.transaction_id
        ))
        .map_err(CashSettlementServiceError::invalid)
    }
}

#[derive(Clone)]
struct CashSettlementCoordinator<L> {
    ledger: L,
}

impl<L: PortfolioLedger> CashSettlementCoordinator<L> {
    fn new(ledger: L) -> Self {
        Self { ledger }
    }

    async fn post(
        &self,
        process: &mut CashSettlementProcess,
        now: DateTime<Utc>,
    ) -> Result<(), CashSettlementServiceError> {
        let result = self
            .ledger
            .record_cash_control_settlement(RecordCashControlSettlement {
                metadata: metadata(process, process.posting_key()?, now, "post")?,
                cash_account_id: process.work.cash_account_id,
                control_account_id: process.control_account_id,
                amount: Money::new(process.work.amount, process.work.currency.clone(), 8)
                    .map_err(CashSettlementServiceError::invalid)?,
                cash_flow: process.work.cash_flow,
                source_operation_id: process.source_operation_id(),
            })
            .await
            .map_err(CashSettlementServiceError::ledger)?;
        if result.cancelled {
            process.state = CashSettlementState::CancelledNoFinancialEffect;
            process.journal_id = None;
        } else {
            process.state = CashSettlementState::Posted;
            process.journal_id = result.journal_entry_id;
        }
        Ok(())
    }

    async fn cancel_or_reverse(
        &self,
        process: &mut CashSettlementProcess,
        reason: String,
        now: DateTime<Utc>,
    ) -> Result<(), CashSettlementServiceError> {
        let result = self
            .ledger
            .cancel_or_reverse_cash_control_settlement(CancelOrReverseCashControlSettlement {
                metadata: metadata(process, process.reversal_key()?, now, "reverse")?,
                source_operation_id: process.source_operation_id(),
                reason,
            })
            .await
            .map_err(CashSettlementServiceError::ledger)?;
        if result.cancelled && result.journal_entry_id.is_none() {
            process.state = CashSettlementState::CancelledNoFinancialEffect;
            process.journal_id = None;
            process.reversal_journal_id = None;
        } else {
            process.state = CashSettlementState::Reversed;
            process.reversal_journal_id = result.journal_entry_id;
        }
        Ok(())
    }
}

fn metadata(
    process: &CashSettlementProcess,
    key: IdempotencyKey,
    now: DateTime<Utc>,
    item: &str,
) -> Result<InternalCommandMetadata, CashSettlementServiceError> {
    Ok(InternalCommandMetadata {
        user_id: process.work.user_id,
        source: SourceReference::new(
            "portfolio",
            format!("transaction:{}", process.work.transaction_id),
            format!("cash-{item}"),
        )
        .map_err(CashSettlementServiceError::invalid)?,
        correlation_id: process.work.correlation_id,
        causation_id: None,
        idempotency_key: key,
        occurred_at: now,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use rust_decimal_macros::dec;

    use super::*;
    use crate::contexts::ledger::public::{
        CashFlowDirection, ControlAccountResult, InternalAccountingResult, ProjectionVersion,
    };
    use crate::contexts::portfolio::public::PortfolioTransactionId;
    use crate::shared_kernel::{CorrelationId, CurrencyCode, UserId};

    #[derive(Clone, Debug, thiserror::Error)]
    #[error("fake failure")]
    struct FakeError;

    #[derive(Clone)]
    struct FakeRepository {
        work: Arc<Mutex<Option<CashSettlementWork>>>,
        completion: Arc<Mutex<Option<CashSettlementCompletion>>>,
        trace: Arc<Mutex<Vec<&'static str>>>,
    }

    #[async_trait]
    impl PortfolioCashSettlementRepository for FakeRepository {
        async fn claim_next(
            &self,
        ) -> Result<Option<CashSettlementWork>, CashSettlementServiceError> {
            self.trace.lock().unwrap().push("claim");
            Ok(self.work.lock().unwrap().take())
        }

        async fn complete(
            &self,
            completion: CashSettlementCompletion,
        ) -> Result<(), CashSettlementServiceError> {
            self.trace.lock().unwrap().push("complete");
            *self.completion.lock().unwrap() = Some(completion);
            Ok(())
        }
    }

    #[derive(Clone)]
    struct FakeLedger {
        trace: Arc<Mutex<Vec<&'static str>>>,
        cancel_without_effect: bool,
    }

    impl PortfolioLedger for FakeLedger {
        type Error = FakeError;

        async fn ensure_typed_control_account(
            &self,
            command: EnsureTypedControlAccount,
        ) -> Result<ControlAccountResult, Self::Error> {
            self.trace.lock().unwrap().push("ensure-control");
            Ok(ControlAccountResult {
                account_id: LedgerAccountId::generate(),
                role: command.role,
                subject_reference: command.subject_reference,
                currency: command.currency,
                replayed: false,
            })
        }

        async fn record_cash_control_settlement(
            &self,
            command: RecordCashControlSettlement,
        ) -> Result<InternalAccountingResult, Self::Error> {
            self.trace.lock().unwrap().push("post");
            Ok(output(
                (!self.cancel_without_effect).then(JournalEntryId::generate),
                self.cancel_without_effect,
                command.metadata.correlation_id,
            ))
        }

        async fn cancel_or_reverse_cash_control_settlement(
            &self,
            command: CancelOrReverseCashControlSettlement,
        ) -> Result<InternalAccountingResult, Self::Error> {
            self.trace.lock().unwrap().push("cancel-or-reverse");
            Ok(output(None, true, command.metadata.correlation_id))
        }
    }

    #[tokio::test]
    async fn claims_before_ledger_effect_and_completes_after_it() {
        let trace = Arc::new(Mutex::new(Vec::new()));
        let completion = Arc::new(Mutex::new(None));
        let transaction_id = PortfolioTransactionId::generate();
        let repository = FakeRepository {
            work: Arc::new(Mutex::new(Some(work(
                transaction_id,
                CashSettlementAction::Post,
            )))),
            completion: Arc::clone(&completion),
            trace: Arc::clone(&trace),
        };
        let service = PortfolioCashSettlementService::new(
            repository,
            FakeLedger {
                trace: Arc::clone(&trace),
                cancel_without_effect: false,
            },
        );

        let report = service.run_once().await.unwrap();
        assert!(report.posted);
        assert_eq!(
            trace.lock().unwrap().as_slice(),
            ["claim", "ensure-control", "post", "complete"]
        );
        let completion = completion.lock().unwrap();
        let completion = completion.as_ref().unwrap();
        assert_eq!(completion.state, CashSettlementState::Posted);
        assert!(completion.journal_id.is_some());
        assert_eq!(completion.work.transaction_id, transaction_id);
    }

    #[tokio::test]
    async fn cancellation_without_a_posted_effect_commits_no_journal() {
        let trace = Arc::new(Mutex::new(Vec::new()));
        let completion = Arc::new(Mutex::new(None));
        let repository = FakeRepository {
            work: Arc::new(Mutex::new(Some(work(
                PortfolioTransactionId::generate(),
                CashSettlementAction::CancelOrReverse,
            )))),
            completion: Arc::clone(&completion),
            trace: Arc::clone(&trace),
        };
        let service = PortfolioCashSettlementService::new(
            repository,
            FakeLedger {
                trace,
                cancel_without_effect: true,
            },
        );

        service.run_once().await.unwrap();
        let completion = completion.lock().unwrap();
        let completion = completion.as_ref().unwrap();
        assert_eq!(
            completion.state,
            CashSettlementState::CancelledNoFinancialEffect
        );
        assert!(completion.journal_id.is_none());
        assert!(completion.reversal_journal_id.is_none());
    }

    fn work(
        transaction_id: PortfolioTransactionId,
        action: CashSettlementAction,
    ) -> CashSettlementWork {
        CashSettlementWork {
            transaction_id,
            user_id: UserId::generate(),
            cash_account_id: LedgerAccountId::generate(),
            amount: dec!(1000),
            currency: CurrencyCode::new("UAH").unwrap(),
            cash_flow: CashFlowDirection::Outgoing,
            correlation_id: CorrelationId::generate(),
            action,
            journal_id: None,
            reversal_journal_id: None,
        }
    }

    fn output(
        journal_entry_id: Option<JournalEntryId>,
        cancelled: bool,
        correlation_id: CorrelationId,
    ) -> InternalAccountingResult {
        InternalAccountingResult {
            journal_entry_id,
            effects: Vec::new(),
            projection_versions: Vec::<ProjectionVersion>::new(),
            replayed: false,
            cancelled,
            outbox_correlation_id: correlation_id,
        }
    }
}
