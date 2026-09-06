use super::*;
use crate::bootstrap::{ContextFacades, build_contexts};
use crate::contexts::classification::public::{
    BackfillRange, ClassifierError, Confidence, DecisionState, Prediction, PredictionReason,
    ReviewAction, UpdateCategoryNode,
};
use crate::contexts::ledger::public::{
    AnnotationChanges, BudgetVisibility, EnableAutomaticClassification, ManualTransactionKind,
    NormalizedTags, OpenAccount, RecordManualTransaction, UpdateTransactionAnnotation,
};
use crate::infrastructure::{database::VerifiedDatabase, test_db::create_fresh_database};
use crate::shared_kernel::{CurrencyCode, Money};
use async_trait::async_trait;
use rust_decimal::Decimal;
use std::sync::atomic::{AtomicUsize, Ordering};
use testcontainers::{ImageExt, runners::AsyncRunner};
use testcontainers_modules::postgres::Postgres;

#[derive(Default)]
struct FakeClassifier {
    calls: AtomicUsize,
}

#[async_trait]
impl TransactionClassifier for FakeClassifier {
    fn provider_name(&self) -> &str {
        "fake"
    }
    fn model_name(&self) -> &str {
        "classification-contract-test"
    }
    async fn classify(
        &self,
        evidence: &ClassificationEvidence,
    ) -> Result<Prediction, ClassifierError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if evidence.description().contains("failure") {
            return Err(ClassifierError::transient("simulated provider failure"));
        }
        let confidence = if evidence.description().contains("low") {
            5_999
        } else if evidence.description().contains("medium") {
            6_000
        } else {
            9_500
        };
        let category = evidence
            .categories()
            .iter()
            .find(|c| c.path().ends_with("Groceries"))
            .unwrap();
        Prediction::new(
            evidence,
            Some(category.id()),
            Confidence::from_basis_points(confidence).unwrap(),
            PredictionReason::DescriptionMatch,
            "Merchant evidence supports this leaf",
        )
        .map_err(|_| ClassifierError::invalid_response())
    }
}

async fn setup() -> (
    VerifiedDatabase,
    ContextFacades,
    ClassificationRuntime,
    Arc<FakeClassifier>,
) {
    let container = Postgres::default()
        .with_tag("16-alpine")
        .start()
        .await
        .unwrap();
    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let database = create_fresh_database(&format!(
        "postgres://postgres:postgres@127.0.0.1:{port}/postgres"
    ))
    .await
    .unwrap()
    .with_lifetime_guard(Arc::new(container));
    let verified = database.initialize().await.unwrap();
    let contexts = build_contexts(&verified);
    let classifier = Arc::new(FakeClassifier::default());
    let runtime = ClassificationRuntime::new(
        verified.pool().clone(),
        contexts.ledger.clone(),
        contexts.banking.clone(),
        contexts.categories.clone(),
        contexts.classification.clone(),
        classifier.clone(),
        true,
    )
    .unwrap();
    (verified, contexts, runtime, classifier)
}

fn key(value: impl Into<String>) -> IdempotencyKey {
    IdempotencyKey::new(value).unwrap()
}

async fn transaction(contexts: &ContextFacades, user_id: UserId, description: &str) -> JournalView {
    let currency = CurrencyCode::new("UAH").unwrap();
    let account = contexts
        .ledger
        .open_account(OpenAccount {
            user_id,
            name: "Test wallet".to_owned(),
            currency: currency.clone(),
            kind: AccountKind::Cash,
            nature: AccountNature::Asset,
            opening_balance: Money::new(Decimal::ZERO, currency.clone(), 2).unwrap(),
            idempotency_key: key(Uuid::new_v4().to_string()),
            correlation_id: CorrelationId::generate(),
            causation_id: None,
            occurred_at: Utc::now(),
        })
        .await
        .unwrap();
    let posted = contexts
        .ledger
        .record_manual_transaction(RecordManualTransaction {
            user_id,
            account_id: account.account.id,
            kind: ManualTransactionKind::Expense,
            amount: Money::new(Decimal::new(12345, 2), currency, 2).unwrap(),
            description: description.to_owned(),
            category_id: None,
            note: Some("private-note-sentinel".to_owned()),
            tags: NormalizedTags::new(vec!["private-tag-sentinel".to_owned()]).unwrap(),
            budget_visibility: BudgetVisibility::Included,
            idempotency_key: key(Uuid::new_v4().to_string()),
            correlation_id: CorrelationId::generate(),
            causation_id: None,
            occurred_at: Utc::now(),
        })
        .await
        .unwrap();
    contexts
        .ledger
        .get_journal(user_id, posted.journal_entry_id)
        .await
        .unwrap()
}

async fn drain(runtime: &ClassificationRuntime) {
    for _ in 0..200 {
        if !runtime.run_intake_once().await.unwrap().claimed {
            return;
        }
    }
    panic!("classification intake did not drain");
}

async fn clear(contexts: &ContextFacades, journal: &JournalView) {
    contexts
        .ledger
        .update_annotation(UpdateTransactionAnnotation {
            user_id: journal.user_id,
            journal_entry_id: journal.id,
            changes: AnnotationChanges {
                category: Some(None),
                ..Default::default()
            },
            expected_version: journal.annotation.as_ref().unwrap().version,
            idempotency_key: key(Uuid::new_v4().to_string()),
            correlation_id: CorrelationId::generate(),
            occurred_at: Utc::now(),
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn review_resolution_is_idempotent_and_updates_reporting_without_balances() {
    let (db, contexts, runtime, fake) = setup().await;
    let user = UserId::generate();
    let journal = transaction(&contexts, user, "Supermarket").await;
    drain(&runtime).await;
    assert!(runtime.run_classifier_once().await.unwrap().claimed);
    assert_eq!(fake.calls.load(Ordering::SeqCst), 1);
    assert!(!runtime.run_classifier_once().await.unwrap().claimed);
    let page = contexts
        .classification
        .review_queue(user, None, 50)
        .await
        .unwrap();
    assert_eq!(page.items.len(), 1); // High confidence remains review while the real rollout gate is closed.
    assert!(
        contexts
            .classification
            .review_queue(UserId::generate(), None, 50)
            .await
            .unwrap()
            .items
            .is_empty()
    );
    let decision = &page.items[0];
    let resolve_key = key("accept-suggestion");
    let applying = contexts
        .classification
        .resolve_review(
            user,
            decision.decision_id,
            ReviewAction::Accept,
            None,
            decision.decision_version,
            decision.annotation_version,
            &resolve_key,
            Utc::now(),
        )
        .await
        .unwrap();
    assert_eq!(applying.state, DecisionState::Applying);
    assert!(runtime.run_application_once().await.unwrap().claimed);
    drain(&runtime).await;
    let assigned = contexts.ledger.get_journal(user, journal.id).await.unwrap();
    let annotation = assigned.annotation.as_ref().unwrap();
    assert_eq!(annotation.assignment_origin, Some(AssignmentOrigin::Manual));
    assert_eq!(annotation.automation_state, AutomationState::Suppressed);
    assert_eq!(
        annotation.category_id.unwrap().into_uuid(),
        decision.candidate_category_id
    );
    assert_eq!(assigned.postings, journal.postings);
    let replay = contexts
        .classification
        .resolve_review(
            user,
            decision.decision_id,
            ReviewAction::Accept,
            None,
            decision.decision_version,
            decision.annotation_version,
            &resolve_key,
            Utc::now(),
        )
        .await
        .unwrap();
    assert_eq!(replay.decision_version, applying.decision_version);
    assert_eq!(
        contexts
            .classification
            .get_decision(user, decision.decision_id)
            .await
            .unwrap()
            .state,
        DecisionState::Accepted
    );
    let reporting = crate::bootstrap::event_consumers(&db);
    for _ in 0..200 {
        if !reporting.run_reporting_once().await.unwrap().claimed {
            break;
        }
    }
    let category: Option<Uuid> = sqlx::query_scalar(
        "SELECT category_id FROM reporting.cashflows WHERE user_id=$1 AND journal_entry_id=$2",
    )
    .bind(user.into_uuid())
    .bind(journal.id.into_uuid())
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(category, Some(decision.candidate_category_id));
    let facts: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM classification.classification_feedback_examples WHERE user_id=$1",
    )
    .bind(user.into_uuid())
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(facts, 1);
}

#[tokio::test]
async fn intake_skips_unrelated_outbox_backlog_for_retry() {
    let (db, contexts, runtime, _) = setup().await;
    let user = UserId::generate();
    let journal = transaction(&contexts, user, "Existing grocery purchase").await;
    drain(&runtime).await;
    let annotation = journal.annotation.unwrap();
    sqlx::query(
        "INSERT INTO integration.outbox_messages(message_id,event_id,message_schema_version,context_name,aggregate_id,aggregate_version,event_type,user_id,occurred_at,correlation_id,payload) VALUES($1,$2,1,'test','unrelated',1,'unrelated.event.v1',$3,clock_timestamp(),$4,'{}')",
    )
    .bind(Uuid::new_v4())
    .bind(Uuid::new_v4())
    .bind(user.into_uuid())
    .bind(Uuid::new_v4())
    .execute(db.pool())
    .await
    .unwrap();
    contexts
        .ledger
        .enable_automatic_classification(EnableAutomaticClassification {
            user_id: user,
            journal_entry_id: journal.id,
            expected_version: annotation.version,
            idempotency_key: key("retry-after-unrelated"),
            correlation_id: CorrelationId::generate(),
            occurred_at: Utc::now(),
        })
        .await
        .unwrap();
    let intake = runtime.run_intake_once().await.unwrap();
    assert_eq!(intake.records, 1);
    assert!(runtime.run_classifier_once().await.unwrap().claimed);
}

#[tokio::test]
async fn manual_clear_and_taxonomy_changes_fence_pending_decisions() {
    let (_db, contexts, runtime, _) = setup().await;
    let user = UserId::generate();
    let journal = transaction(&contexts, user, "Supermarket").await;
    drain(&runtime).await;
    runtime.run_classifier_once().await.unwrap();
    let initial = contexts
        .classification
        .review_queue(user, None, 50)
        .await
        .unwrap()
        .items
        .remove(0);
    let taxonomy = contexts
        .categories
        .taxonomy(user, Utc::now())
        .await
        .unwrap();
    contexts
        .categories
        .update_node(
            UpdateCategoryNode {
                user_id: user,
                idempotency_key: key("rename"),
                id: taxonomy.roots[1].category.id,
                expected_version: taxonomy.version,
                name: Some("My expenses".to_owned()),
                color: None,
                icon: None,
            },
            Utc::now(),
        )
        .await
        .unwrap();
    assert_eq!(
        contexts
            .classification
            .get_decision(user, initial.decision_id)
            .await
            .unwrap()
            .state,
        DecisionState::Stale
    );
    drain(&runtime).await;
    runtime.run_classifier_once().await.unwrap();
    let current = contexts
        .classification
        .review_queue(user, None, 50)
        .await
        .unwrap()
        .items
        .remove(0);
    contexts
        .classification
        .resolve_review(
            user,
            current.decision_id,
            ReviewAction::Accept,
            None,
            current.decision_version,
            current.annotation_version,
            &key("accept"),
            Utc::now(),
        )
        .await
        .unwrap();
    clear(&contexts, &journal).await;
    assert!(runtime.run_application_once().await.unwrap().fenced);
    assert!(
        contexts
            .ledger
            .get_journal(user, journal.id)
            .await
            .unwrap()
            .annotation
            .unwrap()
            .category_id
            .is_none()
    );
}

#[tokio::test]
async fn taxonomy_guard_blocks_mutation_and_application_recovers_after_ledger_commit() {
    let (db, contexts, runtime, _) = setup().await;
    let user = UserId::generate();
    let journal = transaction(&contexts, user, "Supermarket").await;
    drain(&runtime).await;
    runtime.run_classifier_once().await.unwrap();
    let suggestion = contexts
        .classification
        .review_queue(user, None, 50)
        .await
        .unwrap()
        .items
        .remove(0);
    contexts
        .classification
        .resolve_review(
            user,
            suggestion.decision_id,
            ReviewAction::Accept,
            None,
            suggestion.decision_version,
            suggestion.annotation_version,
            &key("accept"),
            Utc::now(),
        )
        .await
        .unwrap();
    let store = contexts.classification.store();
    let claim = store
        .claim_application("crashed", Duration::from_secs(30))
        .await
        .unwrap()
        .unwrap();
    let guard = contexts.categories.assignment_guard(user).await.unwrap();
    contexts
        .ledger
        .apply_category_assignment(ApplyCategoryAssignment {
            user_id: user,
            journal_entry_id: journal.id,
            category_id: claim
                .category_id
                .map(crate::contexts::classification::public::CategoryId::new),
            origin: AssignmentOrigin::Manual,
            classification_decision_id: Some(claim.decision_id.into_uuid()),
            expected_version: AnnotationVersion::new(claim.annotation_version).unwrap(),
            idempotency_key: key(format!("classification-apply-{}", claim.decision_id)),
            correlation_id: CorrelationId::new(claim.decision_id.into_uuid()),
            occurred_at: claim.occurred_at,
        })
        .await
        .unwrap();
    drop(guard); // Simulate death after Ledger commit but before decision completion.
    drain(&runtime).await; // Its own assignment events must not invalidate the pending completion.
    sqlx::query("UPDATE classification.classification_targets SET lease_expires_at=clock_timestamp()-interval '1 second' WHERE user_id=$1")
        .bind(user.into_uuid()).execute(db.pool()).await.unwrap();
    assert!(runtime.run_application_once().await.unwrap().claimed);
    assert_eq!(
        contexts
            .classification
            .get_decision(user, claim.decision_id)
            .await
            .unwrap()
            .state,
        DecisionState::Accepted
    );
    assert_eq!(
        contexts
            .ledger
            .get_journal(user, journal.id)
            .await
            .unwrap()
            .annotation
            .unwrap()
            .version
            .get(),
        2
    );

    let taxonomy = contexts
        .categories
        .taxonomy(user, Utc::now())
        .await
        .unwrap();
    let guard = contexts.categories.assignment_guard(user).await.unwrap();
    let catalog = contexts.categories.clone();
    let mut writer = tokio::spawn(async move {
        catalog
            .update_node(
                UpdateCategoryNode {
                    user_id: user,
                    id: taxonomy.roots[1].category.id,
                    expected_version: taxonomy.version,
                    idempotency_key: key("concurrent-rename"),
                    name: Some("Living costs".to_owned()),
                    color: None,
                    icon: None,
                },
                Utc::now(),
            )
            .await
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut writer)
            .await
            .is_err()
    );
    drop(guard);
    writer.await.unwrap().unwrap();
}

#[tokio::test]
async fn thresholds_failures_and_tenant_private_evidence_are_durable() {
    let (db, contexts, runtime, fake) = setup().await;
    let user = UserId::generate();
    for description in ["high", "medium", "low", "failure"] {
        transaction(&contexts, user, description).await;
    }
    drain(&runtime).await;
    for _ in 0..4 {
        runtime.run_classifier_once().await.unwrap();
    }
    assert_eq!(fake.calls.load(Ordering::SeqCst), 4);
    assert_eq!(
        contexts
            .classification
            .review_queue(user, None, 100)
            .await
            .unwrap()
            .items
            .len(),
        2
    );
    let states: Vec<String> = sqlx::query_scalar(
        "SELECT state FROM classification.classification_targets WHERE user_id=$1 ORDER BY state",
    )
    .bind(user.into_uuid())
    .fetch_all(db.pool())
    .await
    .unwrap();
    assert_eq!(
        states,
        vec!["abstained", "retry_due", "review_pending", "review_pending"]
    );
    let evidence: Vec<Value> = sqlx::query_scalar(
        "SELECT evidence FROM classification.classification_targets WHERE user_id=$1",
    )
    .bind(user.into_uuid())
    .fetch_all(db.pool())
    .await
    .unwrap();
    for value in evidence {
        let serialized = value.to_string();
        assert!(!serialized.contains("private-note-sentinel"));
        assert!(!serialized.contains("private-tag-sentinel"));
        assert!(!serialized.contains(&user.to_string()));
    }
    let attempts: i64 = sqlx::query_scalar("SELECT outbound_calls::bigint FROM classification.classification_daily_usage WHERE user_id=$1")
        .bind(user.into_uuid()).fetch_one(db.pool()).await.unwrap();
    assert_eq!(attempts, 4);
    for _ in 0..4 {
        sqlx::query("UPDATE classification.classification_targets SET next_attempt_at=clock_timestamp() WHERE state='retry_due'")
            .execute(db.pool()).await.unwrap();
        runtime.run_classifier_once().await.unwrap();
    }
    let failures: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM classification.classification_targets WHERE state='failed'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(failures, 1);
}

#[tokio::test]
async fn backfill_enables_legacy_unknown_but_preserves_manual_suppression() {
    let (db, contexts, runtime, _) = setup().await;
    let user = UserId::generate();
    let legacy = transaction(&contexts, user, "Legacy supermarket").await;
    let suppressed = transaction(&contexts, user, "Protected clear").await;
    sqlx::query("UPDATE ledger.transaction_annotations SET automation_state='legacy_unknown' WHERE journal_entry_id=$1")
        .bind(legacy.id.into_uuid()).execute(db.pool()).await.unwrap();
    clear(&contexts, &suppressed).await;
    drain(&runtime).await;
    assert!(!runtime.run_classifier_once().await.unwrap().claimed);
    let range = BackfillRange::new(
        Utc::now() - chrono::Duration::days(1),
        Utc::now() + chrono::Duration::days(1),
    )
    .unwrap();
    let job = contexts
        .classification
        .start_backfill(user, range, &key("history"), Utc::now())
        .await
        .unwrap();
    for _ in 0..5 {
        runtime.run_backfill_once().await.unwrap();
    }
    assert!(runtime.run_classifier_once().await.unwrap().claimed);
    assert!(!runtime.run_classifier_once().await.unwrap().claimed);
    let taxonomy = contexts
        .categories
        .taxonomy(user, Utc::now())
        .await
        .unwrap();
    contexts
        .categories
        .update_node(
            UpdateCategoryNode {
                user_id: user,
                idempotency_key: key("rename-during-backfill"),
                expected_version: taxonomy.version,
                id: taxonomy.roots[1].category.id,
                name: Some("Spending".to_owned()),
                color: None,
                icon: None,
            },
            Utc::now(),
        )
        .await
        .unwrap();
    drain(&runtime).await;
    assert!(runtime.run_classifier_once().await.unwrap().claimed);
    sqlx::query("UPDATE classification.classification_backfill_jobs SET next_resume_at=clock_timestamp() WHERE id=$1")
        .bind(job.id.into_uuid()).execute(db.pool()).await.unwrap();
    runtime.run_backfill_once().await.unwrap();
    let completed = contexts
        .classification
        .get_backfill(user, job.id)
        .await
        .unwrap();
    assert_eq!(
        completed.state,
        crate::contexts::classification::public::BackfillState::Completed
    );
    assert_eq!(completed.review_count, 1);
    assert_eq!(
        contexts
            .ledger
            .get_journal(user, suppressed.id)
            .await
            .unwrap()
            .annotation
            .unwrap()
            .automation_state,
        AutomationState::Suppressed
    );
}

#[tokio::test]
async fn leases_priority_and_daily_quota_survive_restarts() {
    let (db, contexts, runtime, _) = setup().await;
    let user = UserId::generate();
    let historical = transaction(&contexts, user, "Historical").await;
    let live = transaction(&contexts, user, "Live").await;
    let range = BackfillRange::new(
        Utc::now() - chrono::Duration::days(1),
        Utc::now() + chrono::Duration::days(1),
    )
    .unwrap();
    let job = contexts
        .classification
        .start_backfill(user, range, &key("job"), Utc::now())
        .await
        .unwrap();
    let builder = &runtime.intake.evidence;
    let evidence = builder
        .build(user, &historical, None, EvidenceUse::Target)
        .await
        .unwrap()
        .unwrap();
    contexts
        .classification
        .enqueue(&evidence, TargetOrigin::Backfill, Some(job.id))
        .await
        .unwrap();
    let evidence = builder
        .build(user, &live, None, EvidenceUse::Target)
        .await
        .unwrap()
        .unwrap();
    contexts
        .classification
        .enqueue(&evidence, TargetOrigin::Live, None)
        .await
        .unwrap();
    let store = contexts.classification.store();
    let first = store
        .claim_target("first", Duration::from_secs(30))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first.evidence.journal_entry_id(), live.id.into_uuid());
    assert!(
        store
            .reserve_call(&first, "first", 1, Utc::now())
            .await
            .unwrap()
    );
    let second = store
        .claim_target("second", Duration::from_secs(30))
        .await
        .unwrap()
        .unwrap();
    assert!(
        !store
            .reserve_call(&second, "second", 1, Utc::now())
            .await
            .unwrap()
    );
    let resume = store
        .defer_for_quota(&second, "second", Utc::now())
        .await
        .unwrap();
    assert_eq!(resume.time(), chrono::NaiveTime::MIN);
    sqlx::query("UPDATE classification.classification_targets SET lease_expires_at=clock_timestamp()-interval '1 second' WHERE id=$1")
        .bind(first.id.into_uuid()).execute(db.pool()).await.unwrap();
    let restarted = store
        .claim_target("restart", Duration::from_secs(30))
        .await
        .unwrap()
        .unwrap();
    assert!(restarted.lease_token > first.lease_token);
    assert!(matches!(
        store.reserve_call(&first, "first", 100, Utc::now()).await,
        Err(crate::contexts::classification::public::AutomationError::Fenced)
    ));
    assert!(
        store
            .reserve_call(&restarted, "restart", 1, resume)
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn disabling_auto_apply_returns_unapplied_predictions_to_review() {
    let (db, contexts, mut runtime, _) = setup().await;
    let user = UserId::generate();
    let journal = transaction(&contexts, user, "Groceries").await;
    drain(&runtime).await;
    runtime.run_classifier_once().await.unwrap();
    // Simulate a prediction queued while auto-apply was enabled before restart.
    sqlx::query("UPDATE classification.classification_decisions SET state='auto_apply_pending' WHERE user_id=$1")
        .bind(user.into_uuid()).execute(db.pool()).await.unwrap();
    sqlx::query("UPDATE classification.classification_targets SET state='auto_apply_pending' WHERE user_id=$1")
        .bind(user.into_uuid()).execute(db.pool()).await.unwrap();
    runtime.applications.auto_apply_enabled = false;
    assert!(runtime.run_application_once().await.unwrap().claimed);
    let current = contexts.ledger.get_journal(user, journal.id).await.unwrap();
    assert!(current.annotation.unwrap().category_id.is_none());
    let queue = contexts
        .classification
        .review_queue(user, None, 50)
        .await
        .unwrap();
    assert_eq!(queue.items.len(), 1);
    assert!(!runtime.run_application_once().await.unwrap().claimed);
}
