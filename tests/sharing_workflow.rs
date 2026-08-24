use moneykeeper::contexts::ledger::public::{
    AccountKind, AccountNature, ImportProviderTransaction, InternalCommandMetadata, JournalSource,
    OpenAccount, PostingPurpose, ProviderTransactionState, SourceReference,
};
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

mod test_support;

fn metadata(user_id: UserId, key: &str) -> CommandMetadata {
    CommandMetadata {
        user_id,
        idempotency_key: IdempotencyKey::new(key).unwrap(),
        request_hash: canonical_request_hash(&key).unwrap(),
        correlation_id: CorrelationId::generate(),
        occurred_at: chrono::Utc::now(),
    }
}

fn ledger_metadata(user_id: UserId, key: &str) -> InternalCommandMetadata {
    InternalCommandMetadata {
        user_id,
        source: SourceReference::new("test-bank", "checking", key).unwrap(),
        correlation_id: CorrelationId::generate(),
        causation_id: None,
        idempotency_key: IdempotencyKey::new(key).unwrap(),
        occurred_at: chrono::Utc::now(),
    }
}

async fn imported_transaction(
    ledger: &moneykeeper::contexts::ledger::public::LedgerFacade,
    user: UserId,
    account_id: moneykeeper::contexts::ledger::public::LedgerAccountId,
    key: &str,
    amount: i64,
) -> moneykeeper::contexts::ledger::public::JournalEntryId {
    ledger
        .import_provider_transaction(ImportProviderTransaction {
            metadata: ledger_metadata(user, key),
            user_account_id: account_id,
            amount: money(amount),
            state: ProviderTransactionState::Posted,
            description: key.to_owned(),
        })
        .await
        .unwrap()
        .journal_entry_id
        .unwrap()
}

#[tokio::test]
async fn selected_expense_split_and_selected_income_repayment_append_linked_corrections() {
    let (verified, pool) = test_support::fresh_runtime().await;
    let contexts = moneykeeper::bootstrap::build_contexts(&verified);
    let sharing = contexts.sharing.clone();
    let ledger = contexts.ledger.clone();
    let worker = moneykeeper::bootstrap::sharing_workflow_runner(&contexts);
    let competing_worker = moneykeeper::bootstrap::sharing_workflow_runner(&contexts);
    let user = UserId::generate();
    let currency = CurrencyCode::new("UAH").unwrap();
    let cash = ledger
        .open_account(OpenAccount {
            user_id: user,
            name: "Imported checking".to_owned(),
            currency: currency.clone(),
            kind: AccountKind::Current,
            nature: AccountNature::Asset,
            opening_balance: money(100000),
            idempotency_key: IdempotencyKey::new("sharing-checking").unwrap(),
            correlation_id: CorrelationId::generate(),
            causation_id: None,
            occurred_at: chrono::Utc::now(),
        })
        .await
        .unwrap()
        .account
        .id;
    let expense_journal =
        imported_transaction(&ledger, user, cash, "restaurant-expense", -100000).await;
    let contact = sharing
        .create_contact(CreateContact {
            metadata: metadata(user, "sharing-contact"),
            name: ContactName::new("Oksana").unwrap(),
            note: None,
        })
        .await
        .unwrap()
        .contact
        .id;
    let bill = sharing
        .create_bill(CreateBillSplit {
            metadata: metadata(user, "sharing-selected-expense"),
            draft: BillDraft {
                title: "Dinner".into(),
                occurred_at: chrono::Utc::now(),
                total: money(100000),
                minor_unit_scale: 2,
                contributions: vec![
                    Contribution::new(
                        Participant::CurrentUser,
                        money(100000),
                        ContributionEvidence::ExistingJournals {
                            allocations: vec![JournalAllocation {
                                journal_id: LedgerJournalReference::new(
                                    expense_journal.into_uuid(),
                                ),
                                amount: money(100000),
                            }],
                        },
                    )
                    .unwrap(),
                ],
                shares: ShareRequest::Exact(vec![
                    ExactShare {
                        participant: Participant::CurrentUser,
                        amount: money(50000),
                    },
                    ExactShare {
                        participant: Participant::Contact(contact),
                        amount: money(50000),
                    },
                ]),
            },
        })
        .await
        .unwrap()
        .bill;
    assert_eq!(bill.status, BillStatus::PendingAccounting);
    let (first, second) = tokio::join!(worker.run_once(), competing_worker.run_once());
    let first = first.unwrap();
    let second = second.unwrap();
    assert!(
        first.records + second.records == 1
            && u32::from(first.claimed) + u32::from(second.claimed) == 1,
        "concurrent workers did not produce one effective claim: first={first:?}; second={second:?}; bill={:?}",
        sharing.bill(user, bill.id).await.unwrap()
    );

    let active = sharing.bill(user, bill.id).await.unwrap().unwrap();
    assert_eq!(active.status, BillStatus::Active);
    assert_eq!(active.accounted_revision, Some(1));
    assert_eq!(active.accounting_error, None);
    assert!(!active.fully_settled);
    assert_eq!(
        active.allocations["contributions"][0]["evidence"]["allocations"][0]["journal_id"],
        expense_journal.into_uuid().to_string()
    );
    assert_eq!(
        active.allocations["obligations"][0]["remaining_amount"],
        "500.00000000"
    );

    let bill_correction_id: uuid::Uuid = sqlx::query_scalar(
        "SELECT ledger_journal_id FROM sharing.bill_revision_accounting_journals WHERE bill_id=$1 AND user_id=$2 AND revision=1",
    )
    .bind(bill.id.into_uuid())
    .bind(user.into_uuid())
    .fetch_one(&pool)
    .await
    .unwrap();
    let bill_correction = ledger
        .get_journal(
            user,
            moneykeeper::contexts::ledger::public::JournalEntryId::new(bill_correction_id),
        )
        .await
        .unwrap();
    assert_eq!(bill_correction.source, JournalSource::Correction);
    assert_eq!(bill_correction.purpose, PostingPurpose::Correction);
    assert_eq!(bill_correction.relations.corrects(), Some(expense_journal));
    assert!(bill_correction.postings.iter().any(|posting| {
        posting.account_nature == AccountNature::Expense
            && posting.signed_amount == Decimal::new(-50000, 2)
    }));
    assert!(bill_correction.postings.iter().any(|posting| {
        posting.account_nature == AccountNature::Asset
            && posting.signed_amount == Decimal::new(50000, 2)
    }));
    let original_expense = ledger.get_journal(user, expense_journal).await.unwrap();
    assert_eq!(original_expense.source, JournalSource::Import);
    assert_eq!(original_expense.purpose, PostingPurpose::Ordinary);
    assert_eq!(original_expense.reversed_by_journal_id, None);
    assert_eq!(original_expense.replaced_by_journal_id, None);

    let invalid_settlement = sharing
        .create_settlement(CreateSettlement {
            metadata: metadata(user, "sharing-wrong-settlement-direction"),
            bill_id: active.id,
            expected_version: active.version,
            debtor: Participant::Contact(contact),
            creditor: Participant::CurrentUser,
            amount: money(50000),
            evidence: SettlementEvidence::ExistingJournal {
                journal_id: LedgerJournalReference::new(expense_journal.into_uuid()),
            },
        })
        .await
        .unwrap()
        .settlement;
    let failed_report = worker.run_once().await.unwrap();
    assert!(failed_report.claimed && failed_report.records == 0);
    let failed = sharing
        .settlements(user, active.id)
        .await
        .unwrap()
        .into_iter()
        .find(|value| value.id == invalid_settlement.id)
        .unwrap();
    assert_eq!(failed.status, SettlementStatus::Failed);
    assert!(failed.process.last_error.as_deref().is_some_and(|error| {
        error.contains("selected transaction has insufficient unallocated amount")
    }));
    let released = sharing.bill(user, active.id).await.unwrap().unwrap();
    assert_eq!(released.active_settlements, 0);
    assert_eq!(
        released.allocations["obligations"][0]["remaining_amount"],
        "500.00000000"
    );

    let incoming_journal =
        imported_transaction(&ledger, user, cash, "contact-repayment", 50000).await;
    let settlement = sharing
        .create_settlement(CreateSettlement {
            metadata: metadata(user, "sharing-selected-income"),
            bill_id: active.id,
            expected_version: released.version,
            debtor: Participant::Contact(contact),
            creditor: Participant::CurrentUser,
            amount: money(50000),
            evidence: SettlementEvidence::ExistingJournal {
                journal_id: LedgerJournalReference::new(incoming_journal.into_uuid()),
            },
        })
        .await
        .unwrap()
        .settlement;
    assert_eq!(settlement.status, SettlementStatus::PendingAccounting);
    let report = worker.run_once().await.unwrap();
    assert!(
        report.records > 0,
        "worker did not post settlement accounting: {report:?}; settlements={:?}",
        sharing.settlements(user, active.id).await.unwrap()
    );

    let settlements = sharing.settlements(user, active.id).await.unwrap();
    assert_eq!(settlements.len(), 2);
    let posted = settlements
        .iter()
        .find(|value| value.id == settlement.id)
        .unwrap();
    assert_eq!(posted.status, SettlementStatus::Posted);
    assert_eq!(posted.debtor, Some(Participant::Contact(contact)));
    assert_eq!(posted.creditor, Some(Participant::CurrentUser));
    assert_eq!(
        posted.evidence,
        Some(SettlementEvidence::ExistingJournal {
            journal_id: LedgerJournalReference::new(incoming_journal.into_uuid())
        })
    );
    let settlement_correction = ledger
        .get_journal(
            user,
            moneykeeper::contexts::ledger::public::JournalEntryId::new(
                posted.accounting_journal_id.unwrap(),
            ),
        )
        .await
        .unwrap();
    assert_eq!(
        settlement_correction.relations.corrects(),
        Some(incoming_journal)
    );
    assert!(settlement_correction.postings.iter().any(|posting| {
        posting.account_nature == AccountNature::Income
            && posting.signed_amount == Decimal::new(50000, 2)
    }));
    assert!(settlement_correction.postings.iter().any(|posting| {
        posting.account_nature == AccountNature::Asset
            && posting.signed_amount == Decimal::new(-50000, 2)
    }));

    let balances: Vec<(String, Decimal)> = sqlx::query_as(
        "SELECT a.system_role,b.signed_balance FROM ledger.accounts a JOIN ledger.account_balances b ON b.account_id=a.id AND b.user_id=a.user_id WHERE a.user_id=$1 AND a.system_role IN ('uncategorized_expense','uncategorized_income','external_receivable') ORDER BY a.system_role",
    )
    .bind(user.into_uuid())
    .fetch_all(&pool)
    .await
    .unwrap();
    assert!(balances.contains(&("uncategorized_expense".into(), Decimal::new(50000, 2))));
    assert!(balances.contains(&("uncategorized_income".into(), Decimal::ZERO)));
    assert!(balances.contains(&("external_receivable".into(), Decimal::ZERO)));
    let settled_bill = sharing.bill(user, active.id).await.unwrap().unwrap();
    assert!(settled_bill.fully_settled);
    assert_eq!(
        settled_bill.allocations["obligations"][0]["remaining_amount"],
        "0.00000000"
    );

    sharing
        .reverse_settlement(ReverseSettlement {
            metadata: metadata(user, "reverse-selected-income"),
            bill_id: active.id,
            settlement_id: posted.id,
            expected_version: posted.version,
            reason: "Exercise partial replacement".into(),
        })
        .await
        .unwrap();
    assert!(worker.run_once().await.unwrap().records > 0);
    let reopened = sharing.bill(user, active.id).await.unwrap().unwrap();
    assert!(!reopened.fully_settled);
    assert_eq!(
        reopened.allocations["obligations"][0]["remaining_amount"],
        "500.00000000"
    );

    for (index, amount) in [(1, 20000), (2, 30000)] {
        let current = sharing.bill(user, active.id).await.unwrap().unwrap();
        sharing
            .create_settlement(CreateSettlement {
                metadata: metadata(user, &format!("partial-selected-income-{index}")),
                bill_id: active.id,
                expected_version: current.version,
                debtor: Participant::Contact(contact),
                creditor: Participant::CurrentUser,
                amount: money(amount),
                evidence: SettlementEvidence::ExistingJournal {
                    journal_id: LedgerJournalReference::new(incoming_journal.into_uuid()),
                },
            })
            .await
            .unwrap();
        assert!(worker.run_once().await.unwrap().records > 0);
        let current = sharing.bill(user, active.id).await.unwrap().unwrap();
        let expected_remaining = if index == 1 {
            "300.00000000"
        } else {
            "0.00000000"
        };
        assert_eq!(
            current.allocations["obligations"][0]["remaining_amount"],
            expected_remaining
        );
    }
    let final_bill = sharing.bill(user, active.id).await.unwrap().unwrap();
    assert!(final_bill.fully_settled);
    let active_income_capacity: Decimal = sqlx::query_scalar(
        "SELECT COALESCE(sum(d.amount),0) FROM ledger.reclassification_details d WHERE d.user_id=$1 AND d.source_journal_entry_id=$2 AND d.source_nature='income' AND NOT EXISTS(SELECT 1 FROM ledger.journal_entries r WHERE r.user_id=d.user_id AND r.reverses_transaction_id=d.journal_entry_id)",
    )
    .bind(user.into_uuid())
    .bind(incoming_journal.into_uuid())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(active_income_capacity, Decimal::new(50000, 2));

    let crash_source =
        imported_transaction(&ledger, user, cash, "crash-window-expense", -10000).await;
    let crash_bill = sharing
        .create_bill(CreateBillSplit {
            metadata: metadata(user, "crash-window-bill"),
            draft: BillDraft {
                title: "Crash window".into(),
                occurred_at: chrono::Utc::now(),
                total: money(10000),
                minor_unit_scale: 2,
                contributions: vec![
                    Contribution::new(
                        Participant::CurrentUser,
                        money(10000),
                        ContributionEvidence::ExistingJournals {
                            allocations: vec![JournalAllocation {
                                journal_id: LedgerJournalReference::new(crash_source.into_uuid()),
                                amount: money(10000),
                            }],
                        },
                    )
                    .unwrap(),
                ],
                shares: ShareRequest::Exact(vec![
                    ExactShare {
                        participant: Participant::CurrentUser,
                        amount: money(5000),
                    },
                    ExactShare {
                        participant: Participant::Contact(contact),
                        amount: money(5000),
                    },
                ]),
            },
        })
        .await
        .unwrap()
        .bill;
    let claimed = sharing
        .claim_next_work("sharing-crash-window-test")
        .await
        .unwrap()
        .unwrap();
    let claim = match claimed {
        SharingWorkflowWork::BillAccounting { claim, bill, .. } => {
            let outcome = moneykeeper::integration::process_managers::sharing_accounting::SharingAccountingCoordinator::new(ledger.clone())
                .account_manual_revision(&bill)
                .await
                .unwrap();
            assert_eq!(outcome.journal_ids.len(), 1);
            claim
        }
        _ => panic!("expected crash-window bill accounting work"),
    };
    sqlx::query(
        "UPDATE integration.process_leases SET expires_at=clock_timestamp()-interval '1 second' WHERE process_name=$1 AND instance_key=$2",
    )
    .bind(&claim.process_name)
    .bind(&claim.instance_key)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "UPDATE integration.process_instances SET next_wake_at=clock_timestamp()-interval '1 second' WHERE process_name=$1 AND instance_key=$2",
    )
    .bind(&claim.process_name)
    .bind(&claim.instance_key)
    .execute(&pool)
    .await
    .unwrap();
    let recovery_worker = moneykeeper::bootstrap::sharing_workflow_runner(&contexts);
    assert!(recovery_worker.run_once().await.unwrap().records > 0);
    let recovered = sharing.bill(user, crash_bill.id).await.unwrap().unwrap();
    assert_eq!(recovered.status, BillStatus::Active);
    let recovered_corrections: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM ledger.reclassification_details WHERE user_id=$1 AND source_journal_entry_id=$2",
    )
    .bind(user.into_uuid())
    .bind(crash_source.into_uuid())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(recovered_corrections, 1);

    let pending_revision = sharing
        .revise_bill(ReviseBillSplit {
            metadata: metadata(user, "crash-window-revision"),
            bill_id: crash_bill.id,
            expected_version: recovered.version,
            draft: BillDraft {
                title: "Crash window revised".into(),
                occurred_at: chrono::Utc::now(),
                total: money(10000),
                minor_unit_scale: 2,
                contributions: vec![
                    Contribution::new(
                        Participant::CurrentUser,
                        money(10000),
                        ContributionEvidence::ExistingJournals {
                            allocations: vec![JournalAllocation {
                                journal_id: LedgerJournalReference::new(crash_source.into_uuid()),
                                amount: money(10000),
                            }],
                        },
                    )
                    .unwrap(),
                ],
                shares: ShareRequest::Exact(vec![
                    ExactShare {
                        participant: Participant::CurrentUser,
                        amount: money(6000),
                    },
                    ExactShare {
                        participant: Participant::Contact(contact),
                        amount: money(4000),
                    },
                ]),
            },
        })
        .await
        .unwrap()
        .bill;
    assert_eq!(pending_revision.current_revision, 2);
    assert!(recovery_worker.run_once().await.unwrap().records > 0);
    let revised = sharing.bill(user, crash_bill.id).await.unwrap().unwrap();
    assert_eq!(revised.status, BillStatus::Active);
    assert_eq!(revised.accounted_revision, Some(2));
    let revision_rows: Vec<(i32, bool)> = sqlx::query_as(
        "SELECT revision,ledger_reversal_journal_id IS NOT NULL FROM sharing.bill_revision_accounting_journals WHERE bill_id=$1 AND user_id=$2 ORDER BY revision,position",
    )
    .bind(crash_bill.id.into_uuid())
    .bind(user.into_uuid())
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(revision_rows, vec![(1, true), (2, false)]);
    let active_revised_capacity: Decimal = sqlx::query_scalar(
        "SELECT COALESCE(sum(d.amount),0) FROM ledger.reclassification_details d WHERE d.user_id=$1 AND d.source_journal_entry_id=$2 AND d.source_nature='expense' AND NOT EXISTS(SELECT 1 FROM ledger.journal_entries r WHERE r.user_id=d.user_id AND r.reverses_transaction_id=d.journal_entry_id)",
    )
    .bind(user.into_uuid())
    .bind(crash_source.into_uuid())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(active_revised_capacity, Decimal::new(4000, 2));

    sharing
        .cancel_bill(CancelBillSplit {
            metadata: metadata(user, "cancel-revised-crash-window"),
            bill_id: crash_bill.id,
            expected_version: revised.version,
            reason: "Test all-journal cancellation".into(),
        })
        .await
        .unwrap();
    assert!(recovery_worker.run_once().await.unwrap().records > 0);
    let cancelled = sharing.bill(user, crash_bill.id).await.unwrap().unwrap();
    assert_eq!(cancelled.status, BillStatus::Cancelled);
    let all_reversed: bool = sqlx::query_scalar(
        "SELECT bool_and(ledger_reversal_journal_id IS NOT NULL) FROM sharing.bill_revision_accounting_journals WHERE bill_id=$1 AND user_id=$2",
    )
    .bind(crash_bill.id.into_uuid())
    .bind(user.into_uuid())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(all_reversed);
}

#[tokio::test]
async fn durable_contact_to_contact_bill_posts_routes_and_cancels_without_ledger_effect() {
    let (verified, pool) = test_support::fresh_runtime().await;
    let contexts = moneykeeper::bootstrap::build_contexts(&verified);
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
            claim: None,
            user_id: user,
            bill_id: bill.id,
            revision: 1,
            expected_version: bill.version,
            journal_ids: vec![],
            reversed_journals: vec![],
            correlation_id: correlation,
            occurred_at: chrono::Utc::now(),
        })
        .await
        .unwrap();
    assert_eq!(active.status, BillStatus::Active);
    let workers = moneykeeper::bootstrap::event_consumers(&verified);
    workers.run_reporting_once().await.unwrap();
    workers.run_reporting_once().await.unwrap();
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
            claim: None,
            user_id: user,
            bill_id: bill.id,
            expected_version: pending.version,
            reversed_journals: vec![],
            correlation_id: CorrelationId::generate(),
            occurred_at: chrono::Utc::now(),
        })
        .await
        .unwrap();
    assert_eq!(cancelled.status, BillStatus::Cancelled);
    workers.run_reporting_once().await.unwrap();
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
