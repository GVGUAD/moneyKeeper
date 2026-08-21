use moneykeeper::contexts::sharing::public::*;
use moneykeeper::shared_kernel::{CorrelationId, CurrencyCode, IdempotencyKey, Money, UserId};
use rust_decimal::Decimal;
fn money(value: i64) -> Money {
    Money::new(Decimal::new(value, 2), CurrencyCode::new("UAH").unwrap(), 2).unwrap()
}

#[test]
fn multiple_payer_workflow_conserves_every_participant_position() {
    let alice = Participant::Contact(ContactId::new(uuid::Uuid::from_u128(1)));
    let bob = Participant::Contact(ContactId::new(uuid::Uuid::from_u128(2)));
    let carol = Participant::Contact(ContactId::new(uuid::Uuid::from_u128(3)));
    let contributions = vec![
        Contribution::new(
            Participant::CurrentUser,
            money(60000),
            ContributionEvidence::External,
        )
        .unwrap(),
        Contribution::new(alice, money(40000), ContributionEvidence::External).unwrap(),
    ];
    let shares = resolve_allocations(
        &money(100000),
        &contributions,
        ShareRequest::Exact(vec![
            ExactShare {
                participant: Participant::CurrentUser,
                amount: money(10000),
            },
            ExactShare {
                participant: alice,
                amount: money(20000),
            },
            ExactShare {
                participant: bob,
                amount: money(30000),
            },
            ExactShare {
                participant: carol,
                amount: money(40000),
            },
        ]),
        2,
    )
    .unwrap();
    let obligations = derive_obligations(&contributions, &shares, 2).unwrap();
    let paid: Decimal = obligations.iter().map(|value| value.amount.amount()).sum();
    assert_eq!(paid, Decimal::new(70000, 2));
    assert!(
        obligations
            .iter()
            .all(|value| value.debtor != value.creditor)
    );
}

mod v2_test_support;

fn metadata(user_id: UserId, key: &str) -> CommandMetadata {
    CommandMetadata {
        user_id,
        idempotency_key: IdempotencyKey::new(key).unwrap(),
        request_hash: canonical_request_hash(&key).unwrap(),
        correlation_id: CorrelationId::generate(),
        occurred_at: chrono::Utc::now(),
    }
}

#[tokio::test]
async fn durable_contact_to_contact_bill_posts_routes_and_cancels_without_ledger_effect() {
    let (verified, pool) = v2_test_support::fresh_v2_runtime().await;
    let contexts = moneykeeper::bootstrap::v2::supporting_contexts(&verified);
    let sharing = contexts.sharing.clone();
    let user = UserId::generate();
    let alice = sharing
        .create_contact(CreateContact {
            metadata: metadata(user, "alice"),
            name: ContactName::new("Alice").unwrap(),
            note: None,
        })
        .await
        .unwrap()
        .contact
        .id;
    let bob = sharing
        .create_contact(CreateContact {
            metadata: metadata(user, "bob"),
            name: ContactName::new("Bob").unwrap(),
            note: None,
        })
        .await
        .unwrap()
        .contact
        .id;
    let total = money(10000);
    let contributions = vec![
        Contribution::new(
            Participant::Contact(alice),
            total.clone(),
            ContributionEvidence::External,
        )
        .unwrap(),
    ];
    let bill = sharing
        .create_bill(CreateBillSplit {
            metadata: metadata(user, "bill"),
            draft: BillDraft {
                title: "Dinner".into(),
                occurred_at: chrono::Utc::now(),
                total: total.clone(),
                minor_unit_scale: 2,
                contributions,
                shares: ShareRequest::Exact(vec![ExactShare {
                    participant: Participant::Contact(bob),
                    amount: total,
                }]),
            },
        })
        .await
        .unwrap()
        .bill;
    let correlation = CorrelationId::generate();
    let active = sharing
        .complete_bill_accounting(CompleteBillAccounting {
            user_id: user,
            bill_id: bill.id,
            revision: 1,
            expected_version: bill.version,
            journal_id: None,
            correlation_id: correlation,
            occurred_at: chrono::Utc::now(),
        })
        .await
        .unwrap();
    assert_eq!(active.status, BillStatus::Active);
    let workers = moneykeeper::bootstrap::v2::phase4_workers(&verified);
    workers.route_event_once().await.unwrap();
    workers.route_event_once().await.unwrap();
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM reporting.bill_positions WHERE user_id=$1 AND bill_id=$2",
    )
    .bind(user.into_uuid())
    .bind(bill.id.into_uuid())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(count, 1);
    let pending = sharing
        .cancel_bill(CancelBillSplit {
            metadata: metadata(user, "cancel"),
            bill_id: bill.id,
            expected_version: active.version,
            reason: "duplicate".into(),
        })
        .await
        .unwrap()
        .bill;
    let cancelled = sharing
        .complete_bill_cancellation(CompleteBillCancellation {
            user_id: user,
            bill_id: bill.id,
            expected_version: pending.version,
            reversal_journal_id: None,
            correlation_id: CorrelationId::generate(),
            occurred_at: chrono::Utc::now(),
        })
        .await
        .unwrap();
    assert_eq!(cancelled.status, BillStatus::Cancelled);
    workers.route_event_once().await.unwrap();
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM reporting.bill_positions WHERE user_id=$1 AND bill_id=$2",
    )
    .bind(user.into_uuid())
    .bind(bill.id.into_uuid())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(count, 0);
}
