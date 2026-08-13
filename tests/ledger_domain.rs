use chrono::{TimeZone, Utc};
use moneykeeper::contexts::ledger::public::{
    AccountAuthority, AccountKind, AccountLifecycle, AccountNature, AccountVersion, Actor,
    AnnotationChanges, AnnotationId, AnnotationVersion, BalanceObservation, BalanceVersion,
    BudgetVisibility, CategoryReference, JournalEntry, JournalEntryId, JournalRelations,
    JournalSource, LedgerAccount, LedgerAccountId, LedgerError, NormalizedTags, ObservationId,
    Posting, PostingId, PostingPurpose, ReconciliationCase, ReconciliationCaseId,
    ReconciliationStatus, SourceReference, TransactionAnnotation,
};
use moneykeeper::shared_kernel::{
    CausationId, Clock, CorrelationId, CurrencyCode, FixedClock, IdempotencyKey, Money, UserId,
};
use rust_decimal::Decimal;
use uuid::Uuid;

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

fn amount(value: &str, currency: &str) -> Money {
    Money::new(
        Decimal::from_str_exact(value).unwrap(),
        CurrencyCode::new(currency).unwrap(),
        2,
    )
    .unwrap()
}

fn posting(account: &LedgerAccount, signed_amount: &str, purpose: PostingPurpose) -> Posting {
    Posting::for_account(
        PostingId::generate(),
        account,
        Decimal::from_str_exact(signed_amount).unwrap(),
        purpose,
    )
    .unwrap()
}

fn post_journal(
    user_id: UserId,
    postings: Vec<Posting>,
    relations: JournalRelations,
) -> Result<JournalEntry, LedgerError> {
    JournalEntry::post(
        JournalEntryId::generate(),
        user_id,
        "Test entry",
        PostingPurpose::Ordinary,
        JournalSource::Manual,
        Actor::User(user_id),
        Utc.with_ymd_and_hms(2026, 8, 4, 12, 0, 0).unwrap(),
        clock().now(),
        CorrelationId::generate(),
        Some(CausationId::generate()),
        IdempotencyKey::new("journal-test").unwrap(),
        relations,
        postings,
    )
}

#[test]
fn journal_rejects_too_few_zero_mixed_tenant_and_unbalanced_postings() {
    let user = UserId::generate();
    let other = UserId::generate();
    let cash = LedgerAccount::open_manual(
        LedgerAccountId::generate(),
        user,
        "Cash",
        CurrencyCode::new("UAH").unwrap(),
        AccountKind::Cash,
        AccountNature::Asset,
        &clock(),
    )
    .unwrap();
    let savings = LedgerAccount::open_manual(
        LedgerAccountId::generate(),
        user,
        "Savings",
        CurrencyCode::new("UAH").unwrap(),
        AccountKind::Savings,
        AccountNature::Asset,
        &clock(),
    )
    .unwrap();
    let other_cash = LedgerAccount::open_manual(
        LedgerAccountId::generate(),
        other,
        "Other",
        CurrencyCode::new("UAH").unwrap(),
        AccountKind::Cash,
        AccountNature::Asset,
        &clock(),
    )
    .unwrap();

    assert!(
        post_journal(user, vec![], JournalRelations::none())
            .unwrap_err()
            .is_too_few_postings()
    );
    assert!(
        Posting::for_account(
            PostingId::generate(),
            &cash,
            Decimal::ZERO,
            PostingPurpose::Ordinary
        )
        .unwrap_err()
        .is_zero_posting()
    );
    assert!(
        post_journal(
            user,
            vec![
                posting(&cash, "10.00", PostingPurpose::Ordinary),
                posting(&savings, "-9.00", PostingPurpose::Ordinary)
            ],
            JournalRelations::none(),
        )
        .unwrap_err()
        .is_unbalanced_journal()
    );
    assert!(
        post_journal(
            user,
            vec![
                posting(&cash, "10.00", PostingPurpose::Ordinary),
                posting(&other_cash, "-10.00", PostingPurpose::Ordinary)
            ],
            JournalRelations::none(),
        )
        .unwrap_err()
        .is_tenant_mismatch()
    );
}

#[test]
fn journal_balances_each_currency_and_assigns_stable_positions() {
    let user = UserId::generate();
    let uah_a = LedgerAccount::open_manual(
        LedgerAccountId::generate(),
        user,
        "UAH A",
        CurrencyCode::new("UAH").unwrap(),
        AccountKind::Cash,
        AccountNature::Asset,
        &clock(),
    )
    .unwrap();
    let uah_b = LedgerAccount::open_manual(
        LedgerAccountId::generate(),
        user,
        "UAH B",
        CurrencyCode::new("UAH").unwrap(),
        AccountKind::Savings,
        AccountNature::Asset,
        &clock(),
    )
    .unwrap();
    let usd_a = LedgerAccount::open_manual(
        LedgerAccountId::generate(),
        user,
        "USD A",
        CurrencyCode::new("USD").unwrap(),
        AccountKind::Cash,
        AccountNature::Asset,
        &clock(),
    )
    .unwrap();
    let usd_b = LedgerAccount::open_manual(
        LedgerAccountId::generate(),
        user,
        "USD B",
        CurrencyCode::new("USD").unwrap(),
        AccountKind::Savings,
        AccountNature::Asset,
        &clock(),
    )
    .unwrap();
    let ids = [
        PostingId::generate(),
        PostingId::generate(),
        PostingId::generate(),
        PostingId::generate(),
    ];
    let drafts = vec![
        Posting::for_account(
            ids[0],
            &uah_a,
            Decimal::new(-4000, 2),
            PostingPurpose::Ordinary,
        )
        .unwrap(),
        Posting::for_account(
            ids[1],
            &uah_b,
            Decimal::new(4000, 2),
            PostingPurpose::Ordinary,
        )
        .unwrap(),
        Posting::for_account(
            ids[2],
            &usd_a,
            Decimal::new(-100, 2),
            PostingPurpose::Ordinary,
        )
        .unwrap(),
        Posting::for_account(
            ids[3],
            &usd_b,
            Decimal::new(100, 2),
            PostingPurpose::Ordinary,
        )
        .unwrap(),
    ];
    let journal = post_journal(user, drafts, JournalRelations::none()).unwrap();
    assert_eq!(
        journal
            .postings()
            .iter()
            .map(Posting::id)
            .collect::<Vec<_>>(),
        ids
    );
    assert_eq!(
        journal
            .postings()
            .iter()
            .map(Posting::position)
            .collect::<Vec<_>>(),
        vec![1, 2, 3, 4]
    );
}

#[test]
fn journal_relations_are_construction_only_and_archived_repairs_are_allowed() {
    let user = UserId::generate();
    let mut cash = LedgerAccount::open_manual(
        LedgerAccountId::generate(),
        user,
        "Cash",
        CurrencyCode::new("UAH").unwrap(),
        AccountKind::Cash,
        AccountNature::Asset,
        &clock(),
    )
    .unwrap();
    let counter = LedgerAccount::open_manual(
        LedgerAccountId::generate(),
        user,
        "Counter",
        CurrencyCode::new("UAH").unwrap(),
        AccountKind::Savings,
        AccountNature::Asset,
        &clock(),
    )
    .unwrap();
    cash.archive(AccountVersion::INITIAL, &clock()).unwrap();
    assert!(
        Posting::for_account(
            PostingId::generate(),
            &cash,
            Decimal::ONE,
            PostingPurpose::Ordinary
        )
        .unwrap_err()
        .is_account_archived()
    );
    let original = JournalEntryId::generate();
    let relations = JournalRelations::reversal_of(original);
    let journal = JournalEntry::post(
        JournalEntryId::generate(),
        user,
        "Repair",
        PostingPurpose::Reversal,
        JournalSource::Correction,
        Actor::User(user),
        clock().now(),
        clock().now(),
        CorrelationId::generate(),
        None,
        IdempotencyKey::new("repair").unwrap(),
        relations,
        vec![
            posting(&cash, "1.00", PostingPurpose::Reversal),
            posting(&counter, "-1.00", PostingPurpose::Reversal),
        ],
    )
    .unwrap();
    assert_eq!(journal.relations().reverses(), Some(original));
}

#[test]
fn annotations_are_versioned_without_touching_postings() {
    let user = UserId::generate();
    let journal_id = JournalEntryId::generate();
    let mut annotation = TransactionAnnotation::new(
        AnnotationId::generate(),
        journal_id,
        user,
        "Coffee",
        None,
        None,
        NormalizedTags::empty(),
        BudgetVisibility::Included,
        clock().now(),
    )
    .unwrap();
    let category = CategoryReference::new(Uuid::new_v4());
    let changed = annotation
        .update(
            AnnotationChanges {
                description: Some("Morning coffee".to_owned()),
                category: Some(Some(category)),
                note: Some(Some("with team".to_owned())),
                tags: Some(NormalizedTags::new([" Work ", "coffee", "work"]).unwrap()),
                budget_visibility: Some(BudgetVisibility::Excluded),
            },
            AnnotationVersion::INITIAL,
            Actor::User(user),
            clock().now(),
        )
        .unwrap();
    assert!(changed);
    assert_eq!(annotation.version().get(), 2);
    assert_eq!(annotation.tags().as_slice(), ["coffee", "work"]);
    assert_eq!(annotation.category(), Some(category));
    assert_eq!(annotation.audit_events().len(), 1);
    assert_eq!(annotation.journal_entry_id(), journal_id);
    assert!(
        annotation
            .update(
                AnnotationChanges::default(),
                AnnotationVersion::INITIAL,
                Actor::User(user),
                clock().now()
            )
            .unwrap_err()
            .is_version_conflict()
    );
}

#[test]
fn reconciliation_observation_is_visible_and_version_fenced() {
    let user = UserId::generate();
    let account_id = LedgerAccountId::generate();
    let source = SourceReference::new("bank-feed", "resource-1", "balance-7").unwrap();
    let observation = BalanceObservation::new(
        ObservationId::generate(),
        source,
        amount("125.00", "UAH"),
        Some(amount("120.00", "UAH")),
        Utc.with_ymd_and_hms(2026, 8, 5, 9, 0, 0).unwrap(),
        7,
        clock().now(),
    )
    .unwrap();
    let mut case = ReconciliationCase::observe(
        ReconciliationCaseId::generate(),
        user,
        account_id,
        observation,
        amount("100.00", "UAH"),
        BalanceVersion::new(4).unwrap(),
        Actor::System,
        clock().now(),
    )
    .unwrap();
    assert_eq!(case.status(), ReconciliationStatus::Pending);
    assert_eq!(case.delta().amount(), Decimal::new(2500, 2));
    assert!(
        case.approve(
            case.version(),
            BalanceVersion::new(5).unwrap(),
            JournalEntryId::generate(),
            Actor::User(user),
            clock().now(),
        )
        .unwrap_err()
        .is_stale_observed_balance()
    );
    assert_eq!(case.status(), ReconciliationStatus::Pending);

    let matched = ReconciliationCase::observe(
        ReconciliationCaseId::generate(),
        user,
        account_id,
        BalanceObservation::new(
            ObservationId::generate(),
            SourceReference::new("bank-feed", "resource-1", "balance-8").unwrap(),
            amount("100.00", "UAH"),
            None,
            clock().now(),
            8,
            clock().now(),
        )
        .unwrap(),
        amount("100.00", "UAH"),
        BalanceVersion::new(4).unwrap(),
        Actor::System,
        clock().now(),
    )
    .unwrap();
    assert_eq!(matched.status(), ReconciliationStatus::Matched);
    assert!(matched.delta().is_zero());
}
