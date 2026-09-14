use chrono::{TimeZone, Utc};
use moneykeeper::{
    contexts::ledger::{self, public::*},
    shared_kernel::*,
};
use rust_decimal::Decimal;
#[path = "test_support.rs"]
mod test_support;
fn key() -> IdempotencyKey {
    IdempotencyKey::new(uuid::Uuid::new_v4().to_string()).unwrap()
}
fn money(amount: i64) -> Money {
    Money::new(Decimal::from(amount), CurrencyCode::new("UAH").unwrap(), 2).unwrap()
}
async fn open(l: &LedgerFacade, u: UserId, name: &str) -> LedgerAccountId {
    l.open_account(OpenAccount {
        user_id: u,
        name: name.into(),
        currency: CurrencyCode::new("UAH").unwrap(),
        kind: AccountKind::Cash,
        nature: AccountNature::Asset,
        opening_balance: money(1000),
        idempotency_key: key(),
        correlation_id: CorrelationId::generate(),
        causation_id: None,
        occurred_at: Utc::now(),
    })
    .await
    .unwrap()
    .account
    .id
}
async fn ordinary(
    l: &LedgerFacade,
    u: UserId,
    a: LedgerAccountId,
    kind: ManualTransactionKind,
    amount: i64,
    day: u32,
) -> JournalEntryId {
    l.record_manual_transaction(RecordManualTransaction {
        user_id: u,
        account_id: a,
        kind,
        amount: money(amount),
        description: "original description".into(),
        category_id: None,
        note: Some("original note".into()),
        tags: NormalizedTags::empty(),
        budget_visibility: BudgetVisibility::Included,
        idempotency_key: key(),
        correlation_id: CorrelationId::generate(),
        causation_id: None,
        occurred_at: Utc.with_ymd_and_hms(2026, 9, day, 12, 0, 0).unwrap(),
    })
    .await
    .unwrap()
    .journal_entry_id
}
fn response(r: ConversionResponse) -> TransferConversion {
    if let ConversionResponse::Conversion(c) = r {
        c
    } else {
        panic!("conversion expected")
    }
}
#[tokio::test]
async fn linked_fee_conversion_is_atomic_idempotent_and_restores_ordinary_history() {
    let db = test_support::fresh_pool().await;
    let contexts = moneykeeper::bootstrap::build_contexts(&db);
    let l = ledger::build_with_categories(&db, contexts.categories);
    let u = UserId::generate();
    let a = open(&l, u, "source").await;
    let b = open(&l, u, "target").await;
    let outgoing = ordinary(&l, u, a, ManualTransactionKind::Expense, 102, 1).await;
    let incoming = ordinary(&l, u, b, ManualTransactionKind::Income, 100, 3).await;
    let input = ConversionInput {
        other_account_id: a,
        other_journal_id: Some(outgoing),
        missing_side: None,
        fee: Some(ConversionMoney {
            amount: Decimal::from(2),
            currency: CurrencyCode::new("UAH").unwrap(),
        }),
        confirm_fee: true,
        occurred_at: None,
        title: "Move savings".into(),
        note: Some("transfer note".into()),
    };
    let p = match l
        .transfer_conversion(
            u,
            ConversionAction::Preview {
                journal_id: incoming,
                input: input.clone(),
            },
        )
        .await
        .unwrap()
    {
        ConversionResponse::Preview(p) => p,
        _ => panic!(),
    };
    assert_eq!(
        p.occurred_at,
        Utc.with_ymd_and_hms(2026, 9, 1, 12, 0, 0).unwrap()
    );
    assert_eq!(p.outgoing.balance_change, Decimal::ZERO);
    assert_eq!(p.source_principal.amount, Decimal::from(100));
    let action = ConversionAction::Convert {
        journal_id: incoming,
        input,
        version_token: p.version_token,
        key: key(),
    };
    let c = response(l.transfer_conversion(u, action.clone()).await.unwrap());
    assert_eq!(
        c.id,
        response(l.transfer_conversion(u, action).await.unwrap()).id
    );
    assert!(
        l.transfer_conversion(UserId::generate(), ConversionAction::Get { id: c.id })
            .await
            .is_err()
    );
    assert!(
        l.reverse_transaction(ReverseTransaction {
            user_id: u,
            journal_entry_id: c.transfer_journal_id,
            reason: "no".into(),
            idempotency_key: key(),
            correlation_id: CorrelationId::generate(),
            causation_id: None,
            occurred_at: Utc::now()
        })
        .await
        .is_err()
    );
    assert!(l.verify_projection().await.unwrap().is_empty());
    let filter = ActivityFilter::new(
        Utc.with_ymd_and_hms(2026, 9, 1, 0, 0, 0).unwrap(),
        Utc.with_ymd_and_hms(2026, 9, 5, 0, 0, 0).unwrap(),
        ActivityKind::All,
    )
    .unwrap()
    .with_grouped_transfers(true);
    let visible_filter = filter.clone().with_hide_reversed(true);
    let rows = l.list_activity(u, filter.clone(), None, 50).await.unwrap();
    assert_eq!(
        l.list_activity(u, visible_filter.clone(), None, 1)
            .await
            .unwrap(),
        rows
    );
    assert_eq!(
        l.summarize_activity(u, visible_filter.clone())
            .await
            .unwrap()
            .transaction_count,
        1
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].transfer_conversion_id, Some(c.id));
    assert_eq!(
        l.summarize_activity(u, filter.clone())
            .await
            .unwrap()
            .transaction_count,
        1
    );
    let before_a = l.get_account(u, a).await.unwrap();
    let before_b = l.get_account(u, b).await.unwrap();
    l.archive_account(ArchiveAccount {
        user_id: u,
        account_id: a,
        expected_version: before_a.version,
        idempotency_key: key(),
        correlation_id: CorrelationId::generate(),
        occurred_at: Utc::now(),
    })
    .await
    .unwrap();
    let undo = ConversionAction::Undo {
        id: c.id,
        expected_version: c.version,
        key: key(),
    };
    let undone = response(l.transfer_conversion(u, undo.clone()).await.unwrap());
    assert!(!undone.active);
    assert_eq!(undone.restorations.len(), 2);
    assert_eq!(
        response(l.transfer_conversion(u, undo).await.unwrap()).id,
        c.id
    );
    for (original, restored) in &undone.restorations {
        let old = l.get_journal(u, *original).await.unwrap();
        let new = l.get_journal(u, *restored).await.unwrap();
        assert_eq!(new.purpose, PostingPurpose::Ordinary);
        assert_eq!(old.occurred_at, new.occurred_at);
        assert_eq!(old.annotation.unwrap().note, new.annotation.unwrap().note);
    }
    let restored = l.list_activity(u, filter, None, 50).await.unwrap();
    assert_eq!(restored.len(), 2);
    assert_eq!(
        l.list_activity(u, visible_filter.clone(), None, 50)
            .await
            .unwrap(),
        restored
    );
    assert_eq!(
        l.summarize_activity(u, visible_filter)
            .await
            .unwrap()
            .transaction_count,
        2
    );
    assert_eq!(
        l.get_account(u, a).await.unwrap().signed_balance,
        before_a.signed_balance
    );
    assert_eq!(
        l.get_account(u, b).await.unwrap().signed_balance,
        before_b.signed_balance
    );
    l.rebuild_projection().await.unwrap();
    assert!(l.verify_projection().await.unwrap().is_empty());
}
async fn convert_missing(
    l: &LedgerFacade,
    u: UserId,
    j: JournalEntryId,
    other: LedgerAccountId,
    amount: i64,
) -> TransferConversion {
    let input = ConversionInput {
        other_account_id: other,
        other_journal_id: None,
        missing_side: Some(ConversionMoney {
            amount: Decimal::from(amount),
            currency: CurrencyCode::new("UAH").unwrap(),
        }),
        fee: None,
        confirm_fee: false,
        occurred_at: None,
        title: "Missing transfer".into(),
        note: None,
    };
    let p = match l
        .transfer_conversion(
            u,
            ConversionAction::Preview {
                journal_id: j,
                input: input.clone(),
            },
        )
        .await
        .unwrap()
    {
        ConversionResponse::Preview(p) => p,
        _ => panic!(),
    };
    response(
        l.transfer_conversion(
            u,
            ConversionAction::Convert {
                journal_id: j,
                input,
                version_token: p.version_token,
                key: key(),
            },
        )
        .await
        .unwrap(),
    )
}
fn import(u: UserId, a: LedgerAccountId, amount: i64, item: &str) -> ImportProviderTransaction {
    ImportProviderTransaction {
        metadata: InternalCommandMetadata {
            user_id: u,
            source: SourceReference::new("banking", "stream", item).unwrap(),
            idempotency_key: key(),
            correlation_id: CorrelationId::generate(),
            causation_id: None,
            occurred_at: Utc.with_ymd_and_hms(2026, 9, 2, 12, 0, 0).unwrap(),
        },
        user_account_id: a,
        amount: money(amount),
        state: ProviderTransactionState::Posted,
        description: "bank import".into(),
    }
}
#[tokio::test]
async fn matching_import_is_held_confirmed_and_restored_without_duplicate_balance() {
    let db = test_support::fresh_pool().await;
    let contexts = moneykeeper::bootstrap::build_contexts(&db);
    let l = ledger::build_with_categories(&db, contexts.categories);
    let u = UserId::generate();
    let a = open(&l, u, "out").await;
    let b = open(&l, u, "in").await;
    let j = ordinary(&l, u, a, ManualTransactionKind::Expense, 100, 1).await;
    let c = convert_missing(&l, u, j, b, 100).await;
    let before = l.get_account(u, b).await.unwrap().signed_balance;
    let command = import(u, b, 100, "bank-item:1");
    let first = l
        .import_provider_transaction(command.clone())
        .await
        .unwrap();
    assert!(first.journal_entry_id.is_none());
    assert!(
        l.import_provider_transaction(command)
            .await
            .unwrap()
            .journal_entry_id
            .is_none()
    );
    assert_eq!(l.get_account(u, b).await.unwrap().signed_balance, before);
    let reviews = match l
        .transfer_conversion(u, ConversionAction::Reviews { id: None })
        .await
        .unwrap()
    {
        ConversionResponse::Reviews(r) => r,
        _ => panic!(),
    };
    assert_eq!(reviews.len(), 1);
    let review = &reviews[0];
    let action = ConversionAction::ResolveReview {
        id: review.id,
        conversion_id: Some(c.id),
        expected_version: review.version,
        key: key(),
    };
    l.transfer_conversion(u, action.clone()).await.unwrap();
    l.transfer_conversion(u, action).await.unwrap();
    assert_eq!(l.get_account(u, b).await.unwrap().signed_balance, before);
    let c = response(
        l.transfer_conversion(u, ConversionAction::Get { id: c.id })
            .await
            .unwrap(),
    );
    assert_eq!(c.sources.len(), 2);
    let resolved = l
        .transfer_conversion(
            u,
            ConversionAction::ResolveProvider {
                previous: None,
                stream: "stream".into(),
                item: "bank-item:2".into(),
                changed: true,
            },
        )
        .await
        .unwrap();
    let journal = match resolved {
        ConversionResponse::Reference {
            journal_id: Some(j),
        } => j,
        _ => panic!(),
    };
    assert_eq!(
        l.get_journal(u, journal).await.unwrap().purpose,
        PostingPurpose::Ordinary
    );
    let c = response(
        l.transfer_conversion(u, ConversionAction::Get { id: c.id })
            .await
            .unwrap(),
    );
    assert!(!c.active);
    assert_eq!(c.restorations.len(), 2);
    assert_eq!(l.get_account(u, b).await.unwrap().signed_balance, before);
    assert!(l.verify_projection().await.unwrap().is_empty());
}
#[tokio::test]
async fn pending_import_resumes_on_undo_and_dismiss_is_idempotent() {
    let db = test_support::fresh_pool().await;
    let contexts = moneykeeper::bootstrap::build_contexts(&db);
    let l = ledger::build_with_categories(&db, contexts.categories);
    let u = UserId::generate();
    let a = open(&l, u, "out").await;
    let b = open(&l, u, "in").await;
    let j = ordinary(&l, u, b, ManualTransactionKind::Income, 100, 1).await;
    let c = convert_missing(&l, u, j, a, 100).await;
    let command = import(u, a, -100, "outgoing:1");
    assert!(
        l.import_provider_transaction(command.clone())
            .await
            .unwrap()
            .journal_entry_id
            .is_none()
    );
    let before = l.get_account(u, a).await.unwrap().signed_balance;
    l.transfer_conversion(
        u,
        ConversionAction::Undo {
            id: c.id,
            expected_version: c.version,
            key: key(),
        },
    )
    .await
    .unwrap();
    assert_eq!(l.get_account(u, a).await.unwrap().signed_balance, before);
    assert!(
        l.import_provider_transaction(command)
            .await
            .unwrap()
            .journal_entry_id
            .is_some()
    );
    assert_eq!(l.get_account(u, a).await.unwrap().signed_balance, before);
}
#[tokio::test]
async fn stale_preview_and_concurrent_claims_are_rejected() {
    let db = test_support::fresh_pool().await;
    let contexts = moneykeeper::bootstrap::build_contexts(&db);
    let l = ledger::build_with_categories(&db, contexts.categories);
    let u = UserId::generate();
    let a = open(&l, u, "out").await;
    let b = open(&l, u, "in").await;
    let j = ordinary(&l, u, a, ManualTransactionKind::Expense, 100, 1).await;
    let input = ConversionInput {
        other_account_id: b,
        other_journal_id: None,
        missing_side: Some(ConversionMoney {
            amount: Decimal::from(100),
            currency: CurrencyCode::new("UAH").unwrap(),
        }),
        fee: None,
        confirm_fee: false,
        occurred_at: None,
        title: "Transfer".into(),
        note: None,
    };
    let get_preview = || {
        l.transfer_conversion(
            u,
            ConversionAction::Preview {
                journal_id: j,
                input: input.clone(),
            },
        )
    };
    let p = match get_preview().await.unwrap() {
        ConversionResponse::Preview(p) => p,
        _ => panic!(),
    };
    ordinary(&l, u, b, ManualTransactionKind::Income, 1, 4).await;
    let stale = l
        .transfer_conversion(
            u,
            ConversionAction::Convert {
                journal_id: j,
                input: input.clone(),
                version_token: p.version_token,
                key: key(),
            },
        )
        .await
        .unwrap_err();
    assert!(stale.is_version_conflict());
    let p = match get_preview().await.unwrap() {
        ConversionResponse::Preview(p) => p,
        _ => panic!(),
    };
    let action = ConversionAction::Convert {
        journal_id: j,
        input,
        version_token: p.version_token,
        key: key(),
    };
    let (one, two) = tokio::join!(
        l.transfer_conversion(u, action.clone()),
        l.transfer_conversion(u, action)
    );
    assert_eq!(response(one.unwrap()).id, response(two.unwrap()).id);
    assert!(l.verify_projection().await.unwrap().is_empty());
}

#[tokio::test]
async fn fx_target_fee_on_liability_preserves_amounts_and_reports_only_fee() {
    let db = test_support::fresh_pool().await;
    let contexts = moneykeeper::bootstrap::build_contexts(&db);
    let l = ledger::build_with_categories(&db, contexts.categories);
    let u = UserId::generate();
    let a = open(&l, u, "UAH cash").await;
    let usd = CurrencyCode::new("USD").unwrap();
    let b = l
        .open_account(OpenAccount {
            user_id: u,
            name: "USD credit card".into(),
            currency: usd.clone(),
            kind: AccountKind::CreditCard,
            nature: AccountNature::Liability,
            opening_balance: Money::new(Decimal::from(1000), usd.clone(), 2).unwrap(),
            idempotency_key: key(),
            correlation_id: CorrelationId::generate(),
            causation_id: None,
            occurred_at: Utc::now(),
        })
        .await
        .unwrap()
        .account
        .id;
    let out = ordinary(&l, u, a, ManualTransactionKind::Expense, 4000, 1).await;
    let incoming = l
        .import_provider_transaction(ImportProviderTransaction {
            metadata: InternalCommandMetadata {
                user_id: u,
                source: SourceReference::new("banking", "usd", "usd-income:1").unwrap(),
                idempotency_key: key(),
                correlation_id: CorrelationId::generate(),
                causation_id: None,
                occurred_at: Utc.with_ymd_and_hms(2026, 9, 3, 0, 0, 0).unwrap(),
            },
            user_account_id: b,
            amount: Money::new(Decimal::from(99), usd.clone(), 2).unwrap(),
            state: ProviderTransactionState::Posted,
            description: "USD bank income".into(),
        })
        .await
        .unwrap()
        .journal_entry_id
        .unwrap();
    let before = l.get_account(u, b).await.unwrap();
    let input = ConversionInput {
        other_account_id: b,
        other_journal_id: Some(incoming),
        missing_side: None,
        fee: Some(ConversionMoney {
            amount: Decimal::ONE,
            currency: usd.clone(),
        }),
        confirm_fee: false,
        occurred_at: None,
        title: "FX transfer".into(),
        note: None,
    };
    let p = match l
        .transfer_conversion(
            u,
            ConversionAction::Preview {
                journal_id: out,
                input: input.clone(),
            },
        )
        .await
        .unwrap()
    {
        ConversionResponse::Preview(p) => p,
        _ => panic!(),
    };
    assert_eq!(p.source_per_target_rate.as_deref(), Some("40"));
    assert_eq!(p.incoming.signed_amount, Decimal::from(99));
    let c = response(
        l.transfer_conversion(
            u,
            ConversionAction::Convert {
                journal_id: out,
                input,
                version_token: p.version_token,
                key: key(),
            },
        )
        .await
        .unwrap(),
    );
    assert_eq!(
        l.get_account(u, b).await.unwrap().signed_balance,
        before.signed_balance
    );
    let facts = l
        .analytics_aggregate(
            u,
            AnalyticsFilter {
                currency: usd,
                categories: AnalyticsCategories::All,
            },
            vec![AnalyticsInterval {
                from: Utc.with_ymd_and_hms(2026, 9, 1, 0, 0, 0).unwrap(),
                to: Utc.with_ymd_and_hms(2026, 9, 5, 0, 0, 0).unwrap(),
            }],
        )
        .await
        .unwrap();
    assert_eq!(
        facts.iter().map(|f| f.totals.income).sum::<Decimal>(),
        Decimal::ZERO
    );
    assert_eq!(
        facts.iter().map(|f| f.totals.expenses).sum::<Decimal>(),
        Decimal::ONE
    );
    l.transfer_conversion(
        u,
        ConversionAction::Undo {
            id: c.id,
            expected_version: c.version,
            key: key(),
        },
    )
    .await
    .unwrap();
    assert_eq!(
        l.get_account(u, b).await.unwrap().signed_balance,
        before.signed_balance
    );
    assert!(l.verify_projection().await.unwrap().is_empty());
}

#[tokio::test]
async fn conversion_rolls_back_every_journal_projection_and_receipt_on_storage_failure() {
    let (db, pool) = test_support::fresh_runtime().await;
    let contexts = moneykeeper::bootstrap::build_contexts(&db);
    let l = ledger::build_with_categories(&db, contexts.categories);
    let u = UserId::generate();
    let a = open(&l, u, "out").await;
    let b = open(&l, u, "in").await;
    let j = ordinary(&l, u, a, ManualTransactionKind::Expense, 100, 1).await;
    let input = ConversionInput {
        other_account_id: b,
        other_journal_id: None,
        missing_side: Some(ConversionMoney {
            amount: Decimal::from(100),
            currency: CurrencyCode::new("UAH").unwrap(),
        }),
        fee: None,
        confirm_fee: false,
        occurred_at: None,
        title: "Rollback".into(),
        note: None,
    };
    let p = match l
        .transfer_conversion(
            u,
            ConversionAction::Preview {
                journal_id: j,
                input: input.clone(),
            },
        )
        .await
        .unwrap()
    {
        ConversionResponse::Preview(p) => p,
        _ => panic!(),
    };
    sqlx::raw_sql("CREATE FUNCTION ledger.reject_test_conversion() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'injected persistence failure'; END $$; CREATE TRIGGER reject_test_conversion BEFORE INSERT ON ledger.transfer_conversions FOR EACH ROW EXECUTE FUNCTION ledger.reject_test_conversion()").execute(&pool).await.unwrap();
    let action = ConversionAction::Convert {
        journal_id: j,
        input,
        version_token: p.version_token,
        key: key(),
    };
    assert!(
        l.transfer_conversion(u, action.clone())
            .await
            .unwrap_err()
            .is_persistence()
    );
    assert!(
        l.get_journal(u, j)
            .await
            .unwrap()
            .reversed_by_journal_id
            .is_none()
    );
    assert_eq!(
        l.get_account(u, b).await.unwrap().display_balance,
        Decimal::from(1000)
    );
    assert!(l.verify_projection().await.unwrap().is_empty());
    sqlx::query("DROP TRIGGER reject_test_conversion ON ledger.transfer_conversions")
        .execute(&pool)
        .await
        .unwrap();
    l.transfer_conversion(u, action).await.unwrap();
}

#[tokio::test]
async fn already_posted_import_attachment_neutralizes_duplicate_and_restores_both_originals() {
    let db = test_support::fresh_pool().await;
    let contexts = moneykeeper::bootstrap::build_contexts(&db);
    let l = ledger::build_with_categories(&db, contexts.categories);
    let u = UserId::generate();
    let a = open(&l, u, "out").await;
    let b = open(&l, u, "in").await;
    let imported = l
        .import_provider_transaction(import(u, b, 100, "posted:1"))
        .await
        .unwrap()
        .journal_entry_id
        .unwrap();
    let j = ordinary(&l, u, a, ManualTransactionKind::Expense, 100, 1).await;
    let c = convert_missing(&l, u, j, b, 100).await;
    assert_eq!(
        l.get_account(u, b).await.unwrap().signed_balance,
        Decimal::from(1200)
    );
    let action = ConversionAction::Attach {
        id: c.id,
        journal_id: imported,
        expected_version: c.version,
        key: key(),
    };
    let c = response(l.transfer_conversion(u, action.clone()).await.unwrap());
    l.transfer_conversion(u, action).await.unwrap();
    assert_eq!(
        l.get_account(u, b).await.unwrap().signed_balance,
        Decimal::from(1100)
    );
    l.transfer_conversion(
        u,
        ConversionAction::Undo {
            id: c.id,
            expected_version: c.version,
            key: key(),
        },
    )
    .await
    .unwrap();
    assert_eq!(
        l.get_account(u, b).await.unwrap().signed_balance,
        Decimal::from(1100)
    );
    assert!(l.verify_projection().await.unwrap().is_empty());
}
#[tokio::test]
async fn ambiguous_review_requires_an_explicit_match_and_dismiss_posts_once() {
    let db = test_support::fresh_pool().await;
    let contexts = moneykeeper::bootstrap::build_contexts(&db);
    let l = ledger::build_with_categories(&db, contexts.categories);
    let u = UserId::generate();
    let a = open(&l, u, "out").await;
    let b = open(&l, u, "in").await;
    for _ in 0..2 {
        let j = ordinary(&l, u, a, ManualTransactionKind::Expense, 100, 1).await;
        convert_missing(&l, u, j, b, 100).await;
    }
    let command = import(u, b, 100, "ambiguous:1");
    assert!(
        l.import_provider_transaction(command.clone())
            .await
            .unwrap()
            .journal_entry_id
            .is_none()
    );
    let before = l.get_account(u, b).await.unwrap().signed_balance;
    let review = match l
        .transfer_conversion(u, ConversionAction::Reviews { id: None })
        .await
        .unwrap()
    {
        ConversionResponse::Reviews(mut r) => r.pop().unwrap(),
        _ => panic!(),
    };
    assert_eq!(review.candidates.len(), 2);
    assert!(
        l.transfer_conversion(
            u,
            ConversionAction::ResolveReview {
                id: review.id,
                conversion_id: Some(uuid::Uuid::new_v4()),
                expected_version: review.version,
                key: key()
            }
        )
        .await
        .is_err()
    );
    assert_eq!(l.get_account(u, b).await.unwrap().signed_balance, before);
    let dismiss = ConversionAction::ResolveReview {
        id: review.id,
        conversion_id: None,
        expected_version: review.version,
        key: key(),
    };
    l.transfer_conversion(u, dismiss.clone()).await.unwrap();
    l.transfer_conversion(u, dismiss).await.unwrap();
    assert!(
        l.import_provider_transaction(command)
            .await
            .unwrap()
            .journal_entry_id
            .is_some()
    );
    assert_eq!(
        l.get_account(u, b).await.unwrap().signed_balance,
        before + Decimal::from(100)
    );
}
#[tokio::test]
async fn ledger_claim_contract_blocks_preexisting_and_queued_competing_work() {
    let (db, pool) = test_support::fresh_runtime().await;
    let contexts = moneykeeper::bootstrap::build_contexts(&db);
    let l = ledger::build_with_categories(&db, contexts.categories);
    let u = UserId::generate();
    let a = open(&l, u, "out").await;
    let b = open(&l, u, "in").await;
    let j = ordinary(&l, u, a, ManualTransactionKind::Expense, 100, 1).await;
    sqlx::raw_sql("CREATE TABLE recurring.test_conversion_claim(user_id uuid,journal_entry_id uuid,match_id uuid); CREATE TRIGGER claim BEFORE INSERT ON recurring.test_conversion_claim FOR EACH ROW EXECUTE FUNCTION ledger.claim_workflow_journal('journal_entry_id','match_id','recurring')").execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO recurring.test_conversion_claim VALUES($1,$2,$3)")
        .bind(u.into_uuid())
        .bind(j.into_uuid())
        .bind(uuid::Uuid::new_v4())
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        l.transfer_conversion(
            u,
            ConversionAction::Candidates {
                journal_id: j,
                query: ConversionCandidatesQuery::default()
            }
        )
        .await
        .unwrap_err()
        .is_invalid_state()
    );
    sqlx::query("DELETE FROM ledger.workflow_journal_claims WHERE user_id=$1")
        .bind(u.into_uuid())
        .execute(&pool)
        .await
        .unwrap();
    convert_missing(&l, u, j, b, 100).await;
    assert!(
        sqlx::query("INSERT INTO recurring.test_conversion_claim VALUES($1,$2,$3)")
            .bind(u.into_uuid())
            .bind(j.into_uuid())
            .bind(uuid::Uuid::new_v4())
            .execute(&pool)
            .await
            .is_err()
    );
    assert!(l.verify_projection().await.unwrap().is_empty());
}
