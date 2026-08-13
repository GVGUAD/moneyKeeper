use chrono::{TimeZone, Utc};
use moneykeeper::contexts::ledger::public::*;
use moneykeeper::shared_kernel::{
    CausationId, CorrelationId, CurrencyCode, EventId, IdempotencyKey, Money, UserId,
};
use rust_decimal::Decimal;

fn assert_contract<T>() {}

#[test]
fn later_context_commands_are_typed_closed_recipes() {
    assert_contract::<ImportProviderTransaction>();
    assert_contract::<TransitionProviderTransactionState>();
    assert_contract::<ReverseProviderTransaction>();
    assert_contract::<ObserveProviderBalance>();
    assert_contract::<ReclassifyExpenseToReceivableOrPayable>();
    assert_contract::<SettleReceivableOrPayable>();
    assert_contract::<RecordExpenseAndControlBalances>();
    assert_contract::<RecordPrincipalDisbursement>();
    assert_contract::<RecordPrincipalRepayment>();
    assert_contract::<RecordInterestAndFee>();
    assert_contract::<WriteOffLiabilityOrReceivable>();
    assert_contract::<EnsureTypedControlAccount>();
    assert_contract::<RecordCashControlSettlement>();
    assert_contract::<CancelOrReverseCashControlSettlement>();

    let user_id = UserId::generate();
    let source = SourceReference::new("sharing", "expense-stream", "expense-7").unwrap();
    let metadata = InternalCommandMetadata {
        user_id,
        source: source.clone(),
        correlation_id: CorrelationId::generate(),
        causation_id: Some(CausationId::generate()),
        idempotency_key: IdempotencyKey::new("sharing-7").unwrap(),
        occurred_at: Utc.with_ymd_and_hms(2026, 8, 13, 14, 0, 0).unwrap(),
    };
    let command = EnsureTypedControlAccount {
        metadata,
        role: ControlAccountRole::ExternalReceivable,
        subject_reference: "contact:42".to_owned(),
        currency: CurrencyCode::new("UAH").unwrap(),
    };
    assert_eq!(command.metadata.user_id, user_id);
    assert_eq!(command.metadata.source, source);
    assert_eq!(command.role, ControlAccountRole::ExternalReceivable);
    assert_eq!(command.subject_reference, "contact:42");

    // The recipe exposes amounts and selected cash/control identities, never a Posting list,
    // account nature, or caller-selected income/equity counter-account.
    let expense = RecordExpenseAndControlBalances {
        metadata: command.metadata.clone(),
        cash_contributions: vec![CashContribution {
            account_id: LedgerAccountId::generate(),
            amount: Money::new(Decimal::new(600, 2), CurrencyCode::new("UAH").unwrap(), 2).unwrap(),
        }],
        expense: Money::new(Decimal::new(1000, 2), CurrencyCode::new("UAH").unwrap(), 2).unwrap(),
        receivables: vec![],
        payables: vec![ControlAmount {
            account_id: LedgerAccountId::generate(),
            amount: Money::new(Decimal::new(400, 2), CurrencyCode::new("UAH").unwrap(), 2).unwrap(),
        }],
        description: "Dinner".to_owned(),
    };
    assert_eq!(expense.expense.amount(), Decimal::new(1000, 2));
    assert_eq!(expense.cash_contributions.len(), 1);
}

#[test]
fn ledger_event_v1_has_golden_round_trip_for_every_fact_kind() {
    let case = ReconciliationCaseId::generate();
    let journal = JournalEntryId::generate();
    let account = LedgerAccountId::generate();
    let original = JournalEntryId::generate();
    let source = SourceReference::new("loans", "loan:1", "operation:2").unwrap();
    let money = LedgerMoneyV1 {
        amount: Decimal::new(1234, 2),
        currency: CurrencyCode::new("UAH").unwrap(),
    };
    let facts = vec![
        LedgerEventFactV1::AccountLifecycleChanged {
            account_id: account,
            lifecycle: AccountLifecycle::Archived,
        },
        LedgerEventFactV1::EntryPosted {
            journal_entry_id: journal,
            effects: vec![money.clone()],
        },
        LedgerEventFactV1::EntryReversed {
            journal_entry_id: journal,
            original_journal_entry_id: original,
        },
        LedgerEventFactV1::EntryReplaced {
            replacement_journal_entry_id: journal,
            original_journal_entry_id: original,
        },
        LedgerEventFactV1::AnnotationChanged {
            journal_entry_id: journal,
            version: 2,
        },
        LedgerEventFactV1::BalanceChanged {
            account_id: account,
            balance: money,
            version: 3,
        },
        LedgerEventFactV1::ReconciliationObserved { case_id: case },
        LedgerEventFactV1::ReconciliationMatched { case_id: case },
        LedgerEventFactV1::ReconciliationSuperseded { case_id: case },
        LedgerEventFactV1::ReconciliationIgnoredOlder { case_id: case },
        LedgerEventFactV1::ReconciliationApproved {
            case_id: case,
            journal_entry_id: journal,
        },
        LedgerEventFactV1::ReconciliationDismissed { case_id: case },
        LedgerEventFactV1::ReconciliationStale { case_id: case },
        LedgerEventFactV1::InternalAccountingCommandPosted {
            source: source.clone(),
            journal_entry_id: journal,
        },
        LedgerEventFactV1::InternalAccountingCommandFailed {
            source,
            error_code: "invalid_control_account".to_owned(),
        },
    ];
    for (index, fact) in facts.into_iter().enumerate() {
        let event = LedgerEventV1 {
            metadata: LedgerEventMetadataV1 {
                schema_version: 1,
                event_id: EventId::generate(),
                user_id: UserId::generate(),
                sequence: u64::try_from(index + 1).unwrap(),
                correlation_id: CorrelationId::generate(),
                causation_id: Some(CausationId::generate()),
                occurred_at: Utc.with_ymd_and_hms(2026, 8, 13, 15, 0, 0).unwrap(),
                recorded_at: Utc.with_ymd_and_hms(2026, 8, 13, 15, 0, 1).unwrap(),
            },
            fact,
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["metadata"]["schema_version"], 1);
        assert!(json["fact"]["type"].as_str().is_some());
        assert!(!json.to_string().contains("raw_provider"));
        assert_eq!(
            serde_json::from_value::<LedgerEventV1>(json).unwrap(),
            event
        );
    }
}
