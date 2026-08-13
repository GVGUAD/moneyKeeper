use chrono::{TimeZone, Utc};
use moneykeeper::contexts::ledger::public::{
    AccountAuthority, AccountKind, AccountLifecycle, AccountNature, AccountVersion,
    LedgerAccount, LedgerAccountId, LedgerError, PostingPurpose,
};
use moneykeeper::shared_kernel::{CurrencyCode, FixedClock, UserId};

fn clock() -> FixedClock {
    FixedClock::new(Utc.with_ymd_and_hms(2026, 8, 5, 10, 30, 0).unwrap())
}

fn open(kind: AccountKind, nature: AccountNature) -> LedgerAccount {
    LedgerAccount::open_manual(
        LedgerAccountId::generate(),
        UserId::generate(),
        "Everyday",
        CurrencyCode::new("UAH").unwrap(),
        kind,
        nature,
        &clock(),
    )
    .unwrap()
}

#[test]
fn account_manual_asset_and_liability_combinations_are_explicit() {
    let cash = open(AccountKind::Cash, AccountNature::Asset);
    assert_eq!(cash.authority(), AccountAuthority::Manual);
    assert_eq!(cash.lifecycle(), AccountLifecycle::Active);
    assert_eq!(cash.version(), AccountVersion::INITIAL);
    assert!(cash.is_user_visible());

    let card = open(AccountKind::CreditCard, AccountNature::Liability);
    assert_eq!(card.normal_sign(), -1);
    assert_eq!(cash.normal_sign(), 1);

    let payable = open(AccountKind::LoanPayable, AccountNature::Liability);
    let receivable = open(AccountKind::LoanReceivable, AccountNature::Asset);
    assert_eq!(payable.nature(), AccountNature::Liability);
    assert_eq!(receivable.nature(), AccountNature::Asset);
}

#[test]
fn account_rejects_invalid_nature_kind_and_name() {
    assert!(matches!(
        LedgerAccount::open_manual(
            LedgerAccountId::generate(),
            UserId::generate(),
            "Cash",
            CurrencyCode::new("UAH").unwrap(),
            AccountKind::Cash,
            AccountNature::Liability,
            &clock(),
        ),
        Err(error) if error.is_invalid_account_kind()
    ));
    assert!(matches!(
        LedgerAccount::open_manual(
            LedgerAccountId::generate(),
            UserId::generate(),
            "   ",
            CurrencyCode::new("UAH").unwrap(),
            AccountKind::Cash,
            AccountNature::Asset,
            &clock(),
        ),
        Err(error) if error.is_invalid_name()
    ));
}

#[test]
fn account_metadata_changes_are_version_fenced_and_currency_is_immutable() {
    let mut account = open(AccountKind::DebitCard, AccountNature::Asset);
    let original_currency = account.currency().clone();

    account
        .rename("Travel card", AccountVersion::INITIAL, &clock())
        .unwrap();
    assert_eq!(account.name(), "Travel card");
    assert_eq!(account.version().get(), 2);
    assert!(matches!(
        account.rename("Stale", AccountVersion::INITIAL, &clock()),
        Err(error) if error.is_version_conflict()
    ));

    account
        .archive(AccountVersion::new(2).unwrap(), &clock())
        .unwrap();
    assert_eq!(account.lifecycle(), AccountLifecycle::Archived);
    assert!(matches!(
        account.require_posting_allowed(PostingPurpose::Ordinary),
        Err(error) if error.is_account_archived()
    ));
    for purpose in [
        PostingPurpose::Correction,
        PostingPurpose::Reversal,
        PostingPurpose::ApprovedReconciliation,
    ] {
        account.require_posting_allowed(purpose).unwrap();
    }

    account
        .restore(AccountVersion::new(3).unwrap(), &clock())
        .unwrap();
    assert_eq!(account.lifecycle(), AccountLifecycle::Active);
    assert_eq!(account.currency(), &original_currency);
}

#[test]
fn account_version_rejects_non_positive_values() {
    let error: LedgerError = AccountVersion::new(0).unwrap_err();
    assert!(error.is_invalid_version());
}
