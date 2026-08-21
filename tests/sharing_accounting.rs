use moneykeeper::{
    contexts::sharing::public::*,
    integration::process_managers::sharing_accounting::*,
    shared_kernel::{CorrelationId, CurrencyCode, Money, UserId},
};
use rust_decimal::Decimal;

fn money(value: i64) -> Money {
    Money::new(Decimal::new(value, 2), CurrencyCode::new("UAH").unwrap(), 2).unwrap()
}

#[test]
fn accounting_recipe_proves_conservation_for_overpayment() {
    let contact = Participant::Contact(ContactId::generate());
    let contributions = vec![
        Contribution::new(
            Participant::CurrentUser,
            money(60000),
            ContributionEvidence::External,
        )
        .unwrap(),
        Contribution::new(contact, money(40000), ContributionEvidence::External).unwrap(),
    ];
    let shares = vec![
        ParticipantShare {
            participant: Participant::CurrentUser,
            amount: money(10000),
        },
        ParticipantShare {
            participant: contact,
            amount: money(90000),
        },
    ];
    let obligations = derive_obligations(&contributions, &shares, 2).unwrap();
    let revision = BillRevision::new(
        1,
        "Bill",
        chrono::Utc::now(),
        money(100000),
        contributions,
        shares,
        obligations,
        CorrelationId::generate(),
    )
    .unwrap();
    let recipe = AccountingRecipe::from_revision(&revision).unwrap();
    assert_eq!(
        recipe.receivable - recipe.payable,
        recipe.contribution - recipe.share
    );
    assert_eq!(recipe.receivable, Decimal::new(50000, 2));
}

#[test]
fn process_keys_are_stable_and_revision_scoped() {
    let process =
        BillAccountingProcess::start(BillSplitId::generate(), 3, CorrelationId::generate());
    assert!(process.accounting_key().unwrap().as_str().ends_with(":3"));
    assert_eq!(process.state, BillAccountingState::PendingAccounting);
    let _ = UserId::generate();
}
