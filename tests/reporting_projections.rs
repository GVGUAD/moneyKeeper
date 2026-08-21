use chrono::Utc;
use moneykeeper::{
    contexts::{
        ledger::public::{LedgerEventFactV1, LedgerEventMetadataV1, LedgerEventV1, LedgerMoneyV1},
        reporting::public::{ProjectionAction, classify},
    },
    shared_kernel::{CorrelationId, CurrencyCode, EventId, UserId},
};
use rust_decimal_macros::dec;
#[path = "v2_test_support.rs"]
mod v2_test_support;
fn event(version: u32) -> LedgerEventV1 {
    LedgerEventV1 {
        metadata: LedgerEventMetadataV1 {
            schema_version: version,
            event_id: EventId::generate(),
            user_id: UserId::generate(),
            sequence: 1,
            correlation_id: CorrelationId::generate(),
            causation_id: None,
            occurred_at: Utc::now(),
            recorded_at: Utc::now(),
        },
        fact: LedgerEventFactV1::EntryPosted {
            journal_entry_id: moneykeeper::contexts::ledger::public::JournalEntryId::generate(),
            effects: vec![LedgerMoneyV1 {
                amount: dec!(1.00),
                currency: CurrencyCode::new("UAH").unwrap(),
            }],
        },
    }
}
#[test]
fn projector_dispatch_rejects_unknown_major_versions() {
    assert_eq!(classify(&event(1)), Ok(ProjectionAction::JournalPosted));
    assert_eq!(
        classify(&event(2)),
        Err("unknown ledger event major version")
    );
}

#[tokio::test]
async fn reporting_applies_events_exactly_once_and_never_regresses_reconciliation() {
    let (verified, pool) = v2_test_support::fresh_v2_runtime().await;
    let reporting = moneykeeper::bootstrap::v2::supporting_contexts(&verified).reporting;
    let user = UserId::generate();
    let account_id = moneykeeper::contexts::ledger::public::LedgerAccountId::generate();
    let balance_event = LedgerEventV1 {
        metadata: LedgerEventMetadataV1 {
            schema_version: 1,
            event_id: EventId::generate(),
            user_id: user,
            sequence: 1,
            correlation_id: CorrelationId::generate(),
            causation_id: None,
            occurred_at: Utc::now(),
            recorded_at: Utc::now(),
        },
        fact: LedgerEventFactV1::BalanceChanged {
            account_id,
            balance: LedgerMoneyV1 {
                amount: dec!(125.50),
                currency: CurrencyCode::new("UAH").unwrap(),
            },
            version: 2,
        },
    };
    assert!(
        reporting
            .apply_ledger_event(balance_event.clone())
            .await
            .unwrap()
            .applied
    );
    assert!(
        !reporting
            .apply_ledger_event(balance_event)
            .await
            .unwrap()
            .applied
    );

    let case_id = moneykeeper::contexts::ledger::public::ReconciliationCaseId::generate();
    let approved = reconciliation_event(user, case_id, 3, true);
    let observed = reconciliation_event(user, case_id, 2, false);
    reporting.apply_ledger_event(approved).await.unwrap();
    reporting.apply_ledger_event(observed).await.unwrap();

    let state: String = sqlx::query_scalar(
        "SELECT state FROM reporting.reconciliations WHERE user_id=$1 AND case_id=$2",
    )
    .bind(user.into_uuid())
    .bind(case_id.into_uuid())
    .fetch_one(&pool)
    .await
    .unwrap();
    let history: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM reporting.reconciliation_history WHERE user_id=$1 AND case_id=$2",
    )
    .bind(user.into_uuid())
    .bind(case_id.into_uuid())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(state, "approved");
    assert_eq!(history, 2);
}

fn reconciliation_event(
    user: UserId,
    case_id: moneykeeper::contexts::ledger::public::ReconciliationCaseId,
    sequence: u64,
    approved: bool,
) -> LedgerEventV1 {
    LedgerEventV1 {
        metadata: LedgerEventMetadataV1 {
            schema_version: 1,
            event_id: EventId::generate(),
            user_id: user,
            sequence,
            correlation_id: CorrelationId::generate(),
            causation_id: None,
            occurred_at: Utc::now(),
            recorded_at: Utc::now(),
        },
        fact: if approved {
            LedgerEventFactV1::ReconciliationApproved {
                case_id,
                journal_entry_id: moneykeeper::contexts::ledger::public::JournalEntryId::generate(),
            }
        } else {
            LedgerEventFactV1::ReconciliationObserved { case_id }
        },
    }
}
