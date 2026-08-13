#[path = "v2_test_support.rs"]
mod v2_test_support;

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use async_trait::async_trait;
use chrono::Utc;
use moneykeeper::{
    integration::{
        IntegrationEvent,
        inbox::{ConsumerName, InboxError, InboxExecutor, InboxOutcome},
        outbox::{
            DispatcherConfig, EventPublisher, FailureOutcome, OutboxDispatcher, OutboxWriter,
        },
        postgres::{PgInboxExecutor, PgOutboxStore, PgOutboxWriter, PgProcessManagerStore},
        process_manager::{
            ProcessError, ProcessKey, ProcessManagerStore, ProcessState, ProcessStatus,
        },
    },
    shared_kernel::{CorrelationId, EventEnvelope, EventId, UserId},
};
use serde_json::json;
use sqlx::{PgConnection, PgPool, Row};
use tokio::sync::Barrier;
use uuid::Uuid;

fn integration_event() -> IntegrationEvent {
    let envelope = EventEnvelope::new(
        EventId::generate(),
        "test-context",
        Uuid::new_v4().to_string(),
        1,
        "test-context.changed.v1",
        1,
        UserId::generate(),
        Utc::now(),
        CorrelationId::generate(),
        None,
    )
    .unwrap();
    IntegrationEvent::new(envelope, json!({"fact_id": Uuid::new_v4()}))
}

async fn append_committed(pool: &PgPool, event: &IntegrationEvent) {
    let mut transaction = pool.begin().await.unwrap();
    PgOutboxWriter::from_transaction(&mut transaction)
        .append(event)
        .await
        .unwrap();
    transaction.commit().await.unwrap();
}

#[tokio::test]
async fn outbox_append_rolls_back_with_callers_unit_of_work() {
    let (_verified, pool) = v2_test_support::fresh_v2_runtime().await;
    let event = integration_event();

    let mut transaction = pool.begin().await.unwrap();
    PgOutboxWriter::from_transaction(&mut transaction)
        .append(&event)
        .await
        .unwrap();
    transaction.rollback().await.unwrap();

    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM integration.outbox_messages")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);

    append_committed(&pool, &event).await;
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM integration.outbox_messages")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 1);
}

#[tokio::test]
async fn skip_locked_claims_never_give_one_message_to_two_dispatchers() {
    let (verified, pool) = v2_test_support::fresh_v2_runtime().await;
    append_committed(&pool, &integration_event()).await;
    let store = PgOutboxStore::new(&verified);

    let (first, second) = tokio::join!(
        store.claim_batch("dispatcher-a", 1, Duration::from_secs(5)),
        store.claim_batch("dispatcher-b", 1, Duration::from_secs(5)),
    );
    let first = first.unwrap();
    let second = second.unwrap();
    assert_eq!(first.len() + second.len(), 1);
}

#[tokio::test]
async fn crash_after_publish_before_ack_is_redelivered_after_claim_expiry() {
    let (verified, pool) = v2_test_support::fresh_v2_runtime().await;
    let event = integration_event();
    append_committed(&pool, &event).await;
    let store = PgOutboxStore::new(&verified);

    let first = store
        .claim_batch("crashing-dispatcher", 1, Duration::from_millis(20))
        .await
        .unwrap()
        .pop()
        .unwrap();
    // Publication happens here, followed by a process crash: no acknowledgment.
    assert_eq!(first.event.envelope.event_id(), event.envelope.event_id());

    tokio::time::sleep(Duration::from_millis(35)).await;
    let second = store
        .claim_batch("replacement-dispatcher", 1, Duration::from_secs(1))
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(second.event.envelope.event_id(), event.envelope.event_id());
    assert!(second.claim_token > first.claim_token);
    assert_eq!(second.attempts, 2);
    assert!(!store.acknowledge(&first).await.unwrap());
    assert!(store.acknowledge(&second).await.unwrap());
}

#[derive(Clone)]
struct ProbingPublisher {
    pool: PgPool,
    published: Arc<Mutex<Vec<EventId>>>,
    fail: bool,
}

#[derive(Debug, thiserror::Error)]
#[error("access_token=super-secret raw-provider-body")]
struct UnsafePublisherError;

#[async_trait]
impl EventPublisher for ProbingPublisher {
    type Error = UnsafePublisherError;

    async fn publish(&self, event: &IntegrationEvent) -> Result<(), Self::Error> {
        // A separate connection can observe the claim, proving the claim
        // transaction committed before publisher/network I/O began.
        let visible: bool = sqlx::query_scalar(
            "SELECT EXISTS (
                SELECT 1 FROM integration.outbox_messages
                WHERE event_id = $1 AND claim_holder IS NOT NULL
             )",
        )
        .bind(event.envelope.event_id().into_uuid())
        .fetch_one(&self.pool)
        .await
        .unwrap();
        assert!(visible);
        self.published
            .lock()
            .unwrap()
            .push(event.envelope.event_id());
        if self.fail {
            Err(UnsafePublisherError)
        } else {
            Ok(())
        }
    }
}

fn dispatcher_config(maximum_attempts: u32) -> DispatcherConfig {
    DispatcherConfig {
        batch_size: 10,
        claim_ttl: Duration::from_secs(1),
        initial_retry_delay: Duration::from_millis(1),
        maximum_retry_delay: Duration::from_millis(4),
        maximum_attempts,
    }
}

#[tokio::test]
async fn dispatcher_publishes_after_claim_commit_and_acknowledges() {
    let (verified, pool) = v2_test_support::fresh_v2_runtime().await;
    let event = integration_event();
    append_committed(&pool, &event).await;
    let published = Arc::new(Mutex::new(Vec::new()));
    let dispatcher = OutboxDispatcher::new(
        &verified,
        "publisher-1",
        ProbingPublisher {
            pool: pool.clone(),
            published: Arc::clone(&published),
            fail: false,
        },
        dispatcher_config(3),
    )
    .unwrap();

    let report = dispatcher.dispatch_batch().await.unwrap();
    assert_eq!(report.claimed, 1);
    assert_eq!(report.published, 1);
    assert_eq!(
        published.lock().unwrap().as_slice(),
        &[event.envelope.event_id()]
    );

    let published_at: Option<chrono::DateTime<Utc>> = sqlx::query_scalar(
        "SELECT published_at FROM integration.outbox_messages WHERE event_id = $1",
    )
    .bind(event.envelope.event_id().into_uuid())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(published_at.is_some());
}

#[tokio::test]
async fn publication_failures_back_off_redact_and_dead_letter_at_the_cap() {
    let (verified, pool) = v2_test_support::fresh_v2_runtime().await;
    append_committed(&pool, &integration_event()).await;
    let dispatcher = OutboxDispatcher::new(
        &verified,
        "failing-publisher",
        ProbingPublisher {
            pool: pool.clone(),
            published: Arc::new(Mutex::new(Vec::new())),
            fail: true,
        },
        dispatcher_config(2),
    )
    .unwrap();

    let first = dispatcher.dispatch_batch().await.unwrap();
    assert_eq!(first.retry_scheduled, 1);
    let row = sqlx::query(
        "SELECT attempts, available_at > created_at AS backed_off,
                last_error, dead_lettered_at
         FROM integration.outbox_messages",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(row.get::<i32, _>("attempts"), 1);
    assert!(row.get::<bool, _>("backed_off"));
    let error: String = row.get("last_error");
    assert!(error.len() <= 512);
    assert!(!error.contains("super-secret"));
    assert!(
        row.get::<Option<chrono::DateTime<Utc>>, _>("dead_lettered_at")
            .is_none()
    );

    tokio::time::sleep(Duration::from_millis(5)).await;
    let second = dispatcher.dispatch_batch().await.unwrap();
    assert_eq!(second.dead_lettered, 1);
    let dead_lettered: bool =
        sqlx::query_scalar("SELECT dead_lettered_at IS NOT NULL FROM integration.outbox_messages")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(dead_lettered);
}

async fn execute_increment(
    pool: &PgPool,
    consumer: &ConsumerName,
    event: &IntegrationEvent,
) -> Result<InboxOutcome, InboxError> {
    let mut transaction = pool.begin().await.unwrap();
    let outcome = PgInboxExecutor::from_transaction(&mut transaction)
        .execute_once(consumer, event, |connection| {
            Box::pin(async move {
                sqlx::query(
                    "INSERT INTO integration.test_effects (effect_key, executions)
                     VALUES ('effect', 1)
                     ON CONFLICT (effect_key) DO UPDATE
                     SET executions = integration.test_effects.executions + 1",
                )
                .execute(connection)
                .await?;
                Ok(())
            })
        })
        .await?;
    transaction.commit().await.unwrap();
    Ok(outcome)
}

#[tokio::test]
async fn inbox_duplicate_and_concurrent_delivery_apply_local_effect_once() {
    let (_verified, pool) = v2_test_support::fresh_v2_runtime().await;
    sqlx::query(
        "CREATE TABLE integration.test_effects (
            effect_key TEXT PRIMARY KEY,
            executions INTEGER NOT NULL
         )",
    )
    .execute(&pool)
    .await
    .unwrap();
    let event = integration_event();
    let consumer = ConsumerName::new("reporting-projector").unwrap();
    let barrier = Arc::new(Barrier::new(2));

    let first = {
        let pool = pool.clone();
        let event = event.clone();
        let consumer = consumer.clone();
        let barrier = Arc::clone(&barrier);
        tokio::spawn(async move {
            barrier.wait().await;
            execute_increment(&pool, &consumer, &event).await.unwrap()
        })
    };
    let second = {
        let pool = pool.clone();
        let event = event.clone();
        let consumer = consumer.clone();
        let barrier = Arc::clone(&barrier);
        tokio::spawn(async move {
            barrier.wait().await;
            execute_increment(&pool, &consumer, &event).await.unwrap()
        })
    };
    let (first, second) = tokio::join!(first, second);
    let outcomes = [first.unwrap(), second.unwrap()];
    assert!(outcomes.contains(&InboxOutcome::Applied));
    assert!(outcomes.contains(&InboxOutcome::Duplicate));

    let executions: i32 = sqlx::query_scalar(
        "SELECT executions FROM integration.test_effects WHERE effect_key = 'effect'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(executions, 1);

    // Event identity is independently idempotent for another consumer.
    let other = ConsumerName::new("recurring-projector").unwrap();
    assert_eq!(
        execute_increment(&pool, &other, &event).await.unwrap(),
        InboxOutcome::Applied
    );
    let executions: i32 = sqlx::query_scalar(
        "SELECT executions FROM integration.test_effects WHERE effect_key = 'effect'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(executions, 2);
}

#[tokio::test]
async fn failed_inbox_action_rolls_back_receipt_and_partial_effect() {
    let (_verified, pool) = v2_test_support::fresh_v2_runtime().await;
    sqlx::query(
        "CREATE TABLE integration.test_effects (
            effect_key TEXT PRIMARY KEY,
            executions INTEGER NOT NULL
         )",
    )
    .execute(&pool)
    .await
    .unwrap();
    let event = integration_event();
    let consumer = ConsumerName::new("failing-consumer").unwrap();
    let mut transaction = pool.begin().await.unwrap();
    let error = PgInboxExecutor::from_transaction(&mut transaction)
        .execute_once(&consumer, &event, |connection| {
            Box::pin(async move {
                sqlx::query("INSERT INTO integration.test_effects VALUES ('partial', 1)")
                    .execute(connection)
                    .await?;
                Err(InboxError::Action("injected failure".to_owned()))
            })
        })
        .await
        .unwrap_err();
    assert!(matches!(error, InboxError::Action(_)));
    // Even a careless caller commit cannot retain savepoint-local partial work.
    transaction.commit().await.unwrap();

    let receipts: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM integration.inbox_receipts")
        .fetch_one(&pool)
        .await
        .unwrap();
    let effects: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM integration.test_effects")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!((receipts, effects), (0, 0));
}

#[tokio::test]
async fn process_state_compare_and_swap_rejects_stale_versions() {
    let (_verified, pool) = v2_test_support::fresh_v2_runtime().await;
    let key = ProcessKey::new("bank-import", "job-1").unwrap();
    let state = ProcessState::new(
        key.clone(),
        json!({"step": "requested"}),
        ProcessStatus::new("pending").unwrap(),
    );

    let mut transaction = pool.begin().await.unwrap();
    let mut store = PgProcessManagerStore::from_transaction(&mut transaction);
    let lease = store
        .acquire_lease(&key, "worker-a", Duration::from_secs(5))
        .await
        .unwrap();
    store.save(&state, &lease).await.unwrap();
    transaction.commit().await.unwrap();

    let mut transaction = pool.begin().await.unwrap();
    let mut store = PgProcessManagerStore::from_transaction(&mut transaction);
    let error = store.save(&state, &lease).await.unwrap_err();
    assert!(matches!(error, ProcessError::VersionConflict));
    let loaded = store.load(&key).await.unwrap().unwrap();
    assert_eq!(loaded.version(), 1);
    transaction.rollback().await.unwrap();
}

#[tokio::test]
async fn expired_holder_cannot_save_after_successor_gets_higher_fencing_token() {
    let (_verified, pool) = v2_test_support::fresh_v2_runtime().await;
    let key = ProcessKey::new("ledger-posting", "workflow-1").unwrap();
    let state = ProcessState::new(
        key.clone(),
        json!({"step": "requested"}),
        ProcessStatus::new("pending").unwrap(),
    );

    let mut first_transaction = pool.begin().await.unwrap();
    let first = PgProcessManagerStore::from_transaction(&mut first_transaction)
        .acquire_lease(&key, "worker-a", Duration::from_secs(5))
        .await
        .unwrap();
    first_transaction.commit().await.unwrap();

    let mut blocked_transaction = pool.begin().await.unwrap();
    let blocked = PgProcessManagerStore::from_transaction(&mut blocked_transaction)
        .acquire_lease(&key, "worker-b", Duration::from_secs(2))
        .await
        .unwrap_err();
    assert!(matches!(blocked, ProcessError::LeaseUnavailable));
    blocked_transaction.rollback().await.unwrap();
    sqlx::query(
        "UPDATE integration.process_leases
         SET expires_at = clock_timestamp() - interval '1 second'
         WHERE process_name = $1 AND instance_key = $2",
    )
    .bind(key.process_name())
    .bind(key.instance_key())
    .execute(&pool)
    .await
    .unwrap();

    let mut second_transaction = pool.begin().await.unwrap();
    let second = PgProcessManagerStore::from_transaction(&mut second_transaction)
        .acquire_lease(&key, "worker-b", Duration::from_secs(2))
        .await
        .unwrap();
    second_transaction.commit().await.unwrap();
    assert!(second.fencing_token() > first.fencing_token());

    let mut stale_transaction = pool.begin().await.unwrap();
    let error = PgProcessManagerStore::from_transaction(&mut stale_transaction)
        .save(&state, &first)
        .await
        .unwrap_err();
    assert!(matches!(error, ProcessError::LeaseFenced));
    stale_transaction.rollback().await.unwrap();

    let mut current_transaction = pool.begin().await.unwrap();
    PgProcessManagerStore::from_transaction(&mut current_transaction)
        .save(&state, &second)
        .await
        .unwrap();
    current_transaction.commit().await.unwrap();
}

#[tokio::test]
async fn failure_recording_reports_fenced_claim_after_expiry() {
    let (verified, pool) = v2_test_support::fresh_v2_runtime().await;
    append_committed(&pool, &integration_event()).await;
    let store = PgOutboxStore::new(&verified);
    let claim = store
        .claim_batch("slow-worker", 1, Duration::from_millis(10))
        .await
        .unwrap()
        .pop()
        .unwrap();
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(
        store
            .record_failure(&claim, 3, Duration::from_millis(1))
            .await
            .unwrap(),
        FailureOutcome::Fenced
    );
}

// Pins the concrete action signature independently of the database scenarios.
#[allow(dead_code)]
fn inbox_action_contract(
    connection: &mut PgConnection,
) -> moneykeeper::integration::inbox::InboxAction<'_> {
    Box::pin(async move {
        sqlx::query("SELECT 1").execute(connection).await?;
        Ok(())
    })
}
