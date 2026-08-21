mod v2_test_support;

use chrono::{Days, Utc};
use moneykeeper::bootstrap::v2::{phase4_workers, phase6_workers, supporting_contexts};
use moneykeeper::contexts::ledger::public::{AccountKind, AccountNature, OpenAccount};
use moneykeeper::contexts::loans::public::{
    LoanDirection, MovementAmounts, MovementKind, OpenLoan, RecordLoanMovement,
};
use moneykeeper::shared_kernel::{CorrelationId, CurrencyCode, IdempotencyKey, Money, UserId};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

#[tokio::test]
async fn borrowed_loan_posts_components_closes_and_projects_without_principal_income() {
    let pool = v2_test_support::fresh_v2_pool().await;
    let contexts = supporting_contexts(&pool);
    let workers = phase6_workers(&pool);
    let events = phase4_workers(&pool);
    let user = UserId::generate();
    let currency = CurrencyCode::new("UAH").unwrap();
    let now = Utc::now();
    let opened = contexts
        .loans
        .open(OpenLoan {
            user_id: user,
            direction: LoanDirection::Borrowed,
            counterparty: "Alex".to_owned(),
            contractual_principal: dec!(100),
            currency: currency.clone(),
            start_date: now.date_naive(),
            due_date: Some(now.date_naive() + Days::new(365)),
            annual_rate: Some(dec!(10)),
            idempotency_key: key("open"),
            correlation_id: CorrelationId::generate(),
            occurred_at: now,
        })
        .await
        .unwrap();
    assert_eq!(opened.status, "pending_accounting");
    assert!(workers.run_opening_once().await.unwrap().records > 0);
    let loan = contexts
        .loans
        .get(user, opened.agreement_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(loan.version, 2);
    assert!(loan.ledger_principal_account_id.is_some());
    let cash = contexts
        .ledger
        .open_account(OpenAccount {
            user_id: user,
            name: "Cash".to_owned(),
            currency: currency.clone(),
            kind: AccountKind::Cash,
            nature: AccountNature::Asset,
            opening_balance: Money::zero(currency.clone(), 2).unwrap(),
            idempotency_key: key("cash"),
            correlation_id: CorrelationId::generate(),
            causation_id: None,
            occurred_at: now,
        })
        .await
        .unwrap()
        .account
        .id;

    let disbursement = contexts
        .loans
        .record_movement(movement(
            user,
            loan.id,
            MovementKind::Disbursement,
            currency.clone(),
            MovementAmounts {
                principal: dec!(100),
                accrued_interest: Decimal::ZERO,
                accrued_fee: Decimal::ZERO,
                current_interest: Decimal::ZERO,
                current_fee: Decimal::ZERO,
            },
            Some(cash),
            None,
            2,
            "disburse",
        ))
        .await
        .unwrap();
    assert_eq!(disbursement.status, "pending_accounting");
    assert!(workers.run_accounting_once().await.unwrap().records > 0);
    let after_disbursement = contexts.loans.get(user, loan.id).await.unwrap().unwrap();
    assert_eq!(after_disbursement.balances.principal, dec!(100));

    contexts
        .loans
        .record_movement(movement(
            user,
            loan.id,
            MovementKind::Accrual,
            currency.clone(),
            MovementAmounts {
                principal: Decimal::ZERO,
                accrued_interest: dec!(10),
                accrued_fee: Decimal::ZERO,
                current_interest: Decimal::ZERO,
                current_fee: Decimal::ZERO,
            },
            None,
            None,
            after_disbursement.version,
            "accrue",
        ))
        .await
        .unwrap();
    workers.run_accounting_once().await.unwrap();
    let accrued = contexts.loans.get(user, loan.id).await.unwrap().unwrap();
    assert_eq!(accrued.balances.accrued_interest, dec!(10));

    contexts
        .loans
        .record_movement(movement(
            user,
            loan.id,
            MovementKind::Repayment,
            currency,
            MovementAmounts {
                principal: dec!(100),
                accrued_interest: dec!(10),
                accrued_fee: Decimal::ZERO,
                current_interest: Decimal::ZERO,
                current_fee: dec!(5),
            },
            Some(cash),
            None,
            accrued.version,
            "repay",
        ))
        .await
        .unwrap();
    workers.run_accounting_once().await.unwrap();
    let repaid = contexts.loans.get(user, loan.id).await.unwrap().unwrap();
    assert_eq!(repaid.balances.principal, Decimal::ZERO);
    assert_eq!(repaid.balances.accrued_interest, Decimal::ZERO);
    assert_eq!(repaid.balances.accrued_fee, Decimal::ZERO);
    let closed = contexts
        .loans
        .close(
            user,
            loan.id,
            repaid.version,
            key("close"),
            CorrelationId::generate(),
            Utc::now(),
        )
        .await
        .unwrap();
    assert_eq!(closed.status, "closed");

    for _ in 0..40 {
        events.route_event_once().await.unwrap();
    }
    let summary = contexts
        .reporting
        .loan_summary(user, loan.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(summary.principal, Decimal::ZERO);
    assert_eq!(summary.interest, Decimal::ZERO);
    assert_eq!(summary.fees, Decimal::ZERO);
    assert_eq!(summary.status, "closed");
    assert_eq!(summary.direction, Some(LoanDirection::Borrowed));
}

#[allow(clippy::too_many_arguments)]
fn movement(
    user: UserId,
    agreement_id: moneykeeper::contexts::loans::public::LoanAgreementId,
    kind: MovementKind,
    currency: CurrencyCode,
    amounts: MovementAmounts,
    cash_account_id: Option<moneykeeper::contexts::ledger::public::LedgerAccountId>,
    reason: Option<String>,
    expected_version: u64,
    key_value: &str,
) -> RecordLoanMovement {
    RecordLoanMovement {
        user_id: user,
        agreement_id,
        kind,
        currency,
        amounts,
        cash_account_id,
        reason,
        replaces: None,
        expected_version,
        idempotency_key: key(key_value),
        correlation_id: CorrelationId::generate(),
        occurred_at: Utc::now(),
    }
}
fn key(value: &str) -> IdempotencyKey {
    IdempotencyKey::new(format!("phase6-{value}")).unwrap()
}
