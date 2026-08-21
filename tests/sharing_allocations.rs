use chrono::{TimeZone, Utc};
use moneykeeper::contexts::sharing::public::*;
use moneykeeper::shared_kernel::{CorrelationId, CurrencyCode, Money, UserId};
use rust_decimal::Decimal;

fn money(minor: i64) -> Money {
    Money::new(Decimal::new(minor, 2), CurrencyCode::new("UAH").unwrap(), 2).unwrap()
}
fn external(participant: Participant, minor: i64) -> Contribution {
    Contribution::new(participant, money(minor), ContributionEvidence::External).unwrap()
}

#[test]
fn equal_minor_unit_remainder_prefers_current_user_and_obligations_conserve() {
    let a = Participant::Contact(ContactId::new(uuid::Uuid::from_u128(1)));
    let b = Participant::Contact(ContactId::new(uuid::Uuid::from_u128(2)));
    let contributions = vec![external(a, 1)];
    let shares = resolve_allocations(
        &money(1),
        &contributions,
        ShareRequest::Equal(vec![b, a, Participant::CurrentUser]),
        2,
    )
    .unwrap();
    assert_eq!(
        shares
            .iter()
            .find(|v| v.participant == Participant::CurrentUser)
            .unwrap()
            .amount
            .amount(),
        Decimal::new(1, 2)
    );
    assert!(
        shares
            .iter()
            .filter(|v| v.participant != Participant::CurrentUser)
            .all(|v| v.amount.is_zero())
    );
    let obligations = derive_obligations(&contributions, &shares, 2).unwrap();
    assert_eq!(obligations.len(), 1);
    assert_eq!(obligations[0].debtor, Participant::CurrentUser);
    assert_eq!(obligations[0].creditor, a);
    assert_eq!(obligations[0].amount, money(1));
}

#[test]
fn multiple_payers_exact_shares_produce_deterministic_waterfall() {
    let alice = Participant::Contact(ContactId::new(uuid::Uuid::from_u128(1)));
    let bob = Participant::Contact(ContactId::new(uuid::Uuid::from_u128(2)));
    let carol = Participant::Contact(ContactId::new(uuid::Uuid::from_u128(3)));
    let contributions = vec![
        external(Participant::CurrentUser, 60000),
        external(alice, 40000),
    ];
    let exact = vec![
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
    ];
    let shares = resolve_allocations(
        &money(100000),
        &contributions,
        ShareRequest::Exact(exact),
        2,
    )
    .unwrap();
    let obligations = derive_obligations(&contributions, &shares, 2).unwrap();
    assert_eq!(
        obligations
            .iter()
            .map(|v| v.amount.amount())
            .sum::<Decimal>(),
        Decimal::new(70000, 2)
    );
    assert_eq!(
        (
            obligations[0].debtor,
            obligations[0].creditor,
            obligations[0].amount.amount()
        ),
        (bob, Participant::CurrentUser, Decimal::new(30000, 2))
    );
    assert_eq!(
        (
            obligations[1].debtor,
            obligations[1].creditor,
            obligations[1].amount.amount()
        ),
        (carol, Participant::CurrentUser, Decimal::new(20000, 2))
    );
    assert_eq!(
        (
            obligations[2].debtor,
            obligations[2].creditor,
            obligations[2].amount.amount()
        ),
        (carol, alice, Decimal::new(20000, 2))
    );
}

#[test]
fn bill_revision_and_settlement_rules_are_version_fenced() {
    let contact = Participant::Contact(ContactId::generate());
    let contributions = vec![external(Participant::CurrentUser, 1000)];
    let shares = resolve_allocations(
        &money(1000),
        &contributions,
        ShareRequest::Exact(vec![ExactShare {
            participant: contact,
            amount: money(1000),
        }]),
        2,
    )
    .unwrap();
    let obligations = derive_obligations(&contributions, &shares, 2).unwrap();
    let revision = BillRevision::new(
        1,
        "Lunch",
        Utc.with_ymd_and_hms(2026, 8, 5, 12, 0, 0).unwrap(),
        money(1000),
        contributions,
        shares,
        obligations.clone(),
        CorrelationId::generate(),
    )
    .unwrap();
    let mut bill =
        BillSplit::create(BillSplitId::generate(), UserId::generate(), revision).unwrap();
    bill.mark_accounting_posted(BillVersion(1)).unwrap();
    bill.register_settlement(BillVersion(2)).unwrap();
    assert!(matches!(
        bill.request_cancellation("cancel", BillVersion(3)),
        Err(SharingError::ActiveSettlements)
    ));
    let mut settlement = Settlement::create(
        SettlementId::generate(),
        bill.id(),
        bill.user_id(),
        obligations[0].debtor,
        obligations[0].creditor,
        money(500),
        &obligations[0].amount,
        SettlementEvidence::External,
        Utc::now(),
    )
    .unwrap();
    assert!(matches!(
        settlement.reverse("undo", SettlementVersion(1)),
        Err(SharingError::AccountingPending)
    ));
    settlement.mark_posted(SettlementVersion(1)).unwrap();
    settlement.reverse("undo", SettlementVersion(2)).unwrap();
    assert_eq!(settlement.status(), SettlementStatus::Reversed);
    bill.register_settlement_reversal(BillVersion(3)).unwrap();
    bill.request_cancellation("cancel", BillVersion(4)).unwrap();
    assert_eq!(bill.status(), BillStatus::PendingCancellation);
}
