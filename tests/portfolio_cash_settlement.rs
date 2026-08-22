use chrono::Utc;
use moneykeeper::{
    contexts::{ledger::public::*, portfolio::public::*},
    integration::process_managers::portfolio_cash_settlement::*,
    shared_kernel::{CorrelationId, CurrencyCode, UserId},
};
use rust_decimal_macros::dec;
mod v2_test_support;
use std::{
    future::Future,
    sync::{Arc, Mutex},
};

#[derive(Clone, Debug, thiserror::Error)]
#[error("fake failure")]
struct FakeError;
#[derive(Clone, Default)]
struct FakeLedger {
    calls: Arc<Mutex<Vec<String>>>,
    cancelled: Arc<Mutex<bool>>,
}
impl PortfolioLedger for FakeLedger {
    type Error = FakeError;
    fn record_cash_control_settlement(
        &self,
        c: RecordCashControlSettlement,
    ) -> impl Future<Output = Result<InternalAccountingResult, Self::Error>> + Send {
        let this = self.clone();
        async move {
            this.calls.lock().unwrap().push(c.source_operation_id);
            let cancelled = *this.cancelled.lock().unwrap();
            Ok(output(
                if cancelled {
                    None
                } else {
                    Some(JournalEntryId::generate())
                },
                cancelled,
                c.metadata.correlation_id,
            ))
        }
    }
    fn cancel_or_reverse_cash_control_settlement(
        &self,
        c: CancelOrReverseCashControlSettlement,
    ) -> impl Future<Output = Result<InternalAccountingResult, Self::Error>> + Send {
        let this = self.clone();
        async move {
            this.calls.lock().unwrap().push(c.source_operation_id);
            *this.cancelled.lock().unwrap() = true;
            Ok(output(None, true, c.metadata.correlation_id))
        }
    }
}
fn output(
    journal: Option<JournalEntryId>,
    cancelled: bool,
    correlation: CorrelationId,
) -> InternalAccountingResult {
    InternalAccountingResult {
        journal_entry_id: journal,
        effects: vec![],
        projection_versions: vec![],
        replayed: false,
        cancelled,
        outbox_correlation_id: correlation,
    }
}
fn process() -> PortfolioCashSettlementProcess {
    PortfolioCashSettlementProcess {
        transaction_id: PortfolioTransactionId::generate(),
        user_id: UserId::generate(),
        cash_account_id: LedgerAccountId::generate(),
        control_account_id: LedgerAccountId::generate(),
        amount: dec!(1000),
        currency: CurrencyCode::new("UAH").unwrap(),
        cash_flow: CashFlowDirection::Outgoing,
        state: PortfolioCashProcessState::Pending,
        correlation_id: CorrelationId::generate(),
        journal_id: None,
        reversal_journal_id: None,
        last_error: None,
    }
}

#[tokio::test]
async fn posting_uses_stable_source_operation_and_is_idempotent() {
    let ledger = FakeLedger::default();
    let coordinator = PortfolioCashSettlementCoordinator::new(ledger.clone());
    let mut p = process();
    let key = p.source_operation_id();
    coordinator.post(&mut p, Utc::now()).await.unwrap();
    coordinator.post(&mut p, Utc::now()).await.unwrap();
    assert_eq!(p.state, PortfolioCashProcessState::Posted);
    assert_eq!(ledger.calls.lock().unwrap().as_slice(), [key]);
}
#[tokio::test]
async fn reversal_before_post_cancels_without_fabricating_a_journal() {
    let ledger = FakeLedger::default();
    let coordinator = PortfolioCashSettlementCoordinator::new(ledger);
    let mut p = process();
    coordinator
        .cancel_or_reverse(&mut p, "cancel".into(), Utc::now())
        .await
        .unwrap();
    coordinator.post(&mut p, Utc::now()).await.unwrap();
    assert_eq!(
        p.state,
        PortfolioCashProcessState::CancelledNoFinancialEffect
    );
    assert!(p.journal_id.is_none());
}

#[tokio::test]
async fn durable_worker_posts_and_reverses_one_correlated_ledger_effect() {
    let (verified, pool) = v2_test_support::fresh_v2_runtime().await;
    let contexts = moneykeeper::bootstrap::v2::supporting_contexts(&verified);
    let user = UserId::generate();
    let now = Utc::now();
    let currency = CurrencyCode::new("UAH").unwrap();
    let cash = contexts
        .ledger
        .open_account(OpenAccount {
            user_id: user,
            name: "Cash".into(),
            currency: currency.clone(),
            kind: AccountKind::Cash,
            nature: AccountNature::Asset,
            opening_balance: moneykeeper::shared_kernel::Money::new(
                dec!(5000),
                currency.clone(),
                2,
            )
            .unwrap(),
            idempotency_key: moneykeeper::shared_kernel::IdempotencyKey::new(
                "portfolio-cash-account",
            )
            .unwrap(),
            correlation_id: CorrelationId::generate(),
            causation_id: None,
            occurred_at: now,
        })
        .await
        .unwrap();
    let instrument = contexts
        .portfolio
        .create_manual_ovdp(CreateManualOvdpInstrument {
            user_id: user,
            identifier: InstrumentIdentifier::new(IdentifierKind::Manual, "CASH-OVDP").unwrap(),
            display_name: "Cash ОВДП".into(),
            currency: currency.clone(),
            face_value: dec!(1000),
            issue_date: now.date_naive(),
            maturity_date: now.date_naive() + chrono::Days::new(365),
            coupon_terms: CouponTerms::ZeroCoupon,
            idempotency_key: moneykeeper::shared_kernel::IdempotencyKey::new("cash-instrument")
                .unwrap(),
            correlation_id: CorrelationId::generate(),
            occurred_at: now,
        })
        .await
        .unwrap();
    let account = contexts
        .portfolio
        .open_account(OpenPortfolioAccount {
            user_id: user,
            name: "Treasury".into(),
            idempotency_key: moneykeeper::shared_kernel::IdempotencyKey::new(
                "cash-portfolio-account",
            )
            .unwrap(),
            correlation_id: CorrelationId::generate(),
            occurred_at: now,
        })
        .await
        .unwrap();
    let account_id = PortfolioAccountId::new(account.aggregate_id);
    let instrument_id = InstrumentId::new(instrument.aggregate_id);
    let purchase = contexts
        .portfolio
        .record_transaction(RecordPortfolioTransaction {
            user_id: user,
            account_id,
            instrument_id,
            expected_account_version: 1,
            expected_position_version: 0,
            activity: PortfolioActivityCommand::Buy {
                quantity: dec!(1),
                total_acquisition_cost: dec!(1000),
                fee: None,
                accrued_interest: None,
                trade_at: now,
            },
            cash_settlement: Some(OptionalCashSettlement {
                cash_account_id: cash.account.id,
                amount: dec!(1000),
            }),
            actor_id: PortfolioActorId::generate(),
            idempotency_key: moneykeeper::shared_kernel::IdempotencyKey::new("cash-buy").unwrap(),
            correlation_id: CorrelationId::generate(),
            recorded_at: now,
        })
        .await
        .unwrap();
    let workers = moneykeeper::bootstrap::v2::phase7_workers(&verified);
    assert!(workers.run_cash_once().await.unwrap().records == 1);
    assert_eq!(
        sqlx::query_scalar::<_, String>(
            "SELECT state FROM portfolio.cash_settlement_processes WHERE transaction_id=$1"
        )
        .bind(purchase.transaction_id.unwrap().into_uuid())
        .fetch_one(&pool)
        .await
        .unwrap(),
        "posted"
    );
    contexts
        .portfolio
        .reverse_transaction(ReversePortfolioTransaction {
            user_id: user,
            transaction_id: purchase.transaction_id.unwrap(),
            expected_account_version: 1,
            expected_position_version: 1,
            reason: "Cancelled purchase".into(),
            actor_id: PortfolioActorId::generate(),
            idempotency_key: moneykeeper::shared_kernel::IdempotencyKey::new("cash-buy-reversal")
                .unwrap(),
            correlation_id: CorrelationId::generate(),
            recorded_at: now,
        })
        .await
        .unwrap();
    workers.run_cash_once().await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, String>(
            "SELECT state FROM portfolio.cash_settlement_processes WHERE transaction_id=$1"
        )
        .bind(purchase.transaction_id.unwrap().into_uuid())
        .fetch_one(&pool)
        .await
        .unwrap(),
        "reversed"
    );
}
