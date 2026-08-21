#[path = "v2_test_support.rs"]
mod v2_test_support;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use chrono::{TimeZone, Utc};
use moneykeeper::bootstrap::v2::supporting_contexts;
use moneykeeper::contexts::classification::public::{
    CategoryCatalog, CategoryCommand, CategoryKind,
};
use moneykeeper::contexts::preferences::public::Preferences;
use moneykeeper::contexts::reference_data::public::CurrencyCatalog;
use moneykeeper::integration::IntegrationEvent;
use moneykeeper::integration::inbox::{ConsumerName, InboxExecutor, InboxOutcome};
use moneykeeper::integration::outbox::{
    DispatcherConfig, EventPublisher, OutboxDispatcher, OutboxWriter,
};
use moneykeeper::integration::postgres::{PgInboxExecutor, PgOutboxWriter, PgProcessManagerStore};
use moneykeeper::integration::process_manager::{
    ProcessError, ProcessKey, ProcessManagerStore, ProcessState, ProcessStatus,
};
use moneykeeper::shared_kernel::{CorrelationId, CurrencyCode, EventEnvelope, EventId, UserId};
use serde_json::json;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

fn representative_event(user_id: UserId, probe_id: Uuid) -> IntegrationEvent {
    IntegrationEvent::new(
        EventEnvelope::new(
            EventId::generate(),
            "ledger",
            probe_id.to_string(),
            1,
            "ledger.foundation-probed.v1",
            1,
            user_id,
            Utc.with_ymd_and_hms(2026, 8, 13, 12, 0, 0)
                .single()
                .unwrap(),
            CorrelationId::generate(),
            None,
        )
        .unwrap(),
        json!({"probe_id": probe_id}),
    )
}

#[derive(Clone)]
struct RecordingPublisher {
    deliveries: Arc<Mutex<Vec<EventId>>>,
    fail_after_publish: bool,
}

#[derive(Debug, thiserror::Error)]
#[error("simulated crash after publication")]
struct SimulatedCrash;

#[async_trait]
impl EventPublisher for RecordingPublisher {
    type Error = SimulatedCrash;

    async fn publish(&self, event: &IntegrationEvent) -> Result<(), Self::Error> {
        self.deliveries
            .lock()
            .unwrap()
            .push(event.envelope.event_id());
        if self.fail_after_publish {
            Err(SimulatedCrash)
        } else {
            Ok(())
        }
    }
}

fn dispatcher_config() -> DispatcherConfig {
    DispatcherConfig {
        batch_size: 10,
        claim_ttl: Duration::from_secs(1),
        initial_retry_delay: Duration::from_millis(1),
        maximum_retry_delay: Duration::from_millis(5),
        maximum_attempts: 3,
    }
}

async fn consume_once(
    pool: &sqlx::PgPool,
    consumer: &ConsumerName,
    event: &IntegrationEvent,
) -> InboxOutcome {
    let mut transaction = pool.begin().await.unwrap();
    let outcome = PgInboxExecutor::from_transaction(&mut transaction)
        .execute_once(consumer, event, |connection: &mut PgConnection| {
            Box::pin(async move {
                sqlx::query(
                    "INSERT INTO reporting.foundation_effects (effect_key, executions) \
                     VALUES ('projection', 1) \
                     ON CONFLICT (effect_key) DO UPDATE \
                     SET executions = reporting.foundation_effects.executions + 1",
                )
                .execute(connection)
                .await?;
                Ok(())
            })
        })
        .await
        .unwrap();
    transaction.commit().await.unwrap();
    outcome
}

#[tokio::test]
async fn isolated_finance_v2_foundation_composes_end_to_end() {
    let database = v2_test_support::fresh_v2_database().await;
    let verified = database.initialize().await.unwrap();
    let pool = PgPool::connect(database.database_url()).await.unwrap();
    let migration_versions: Vec<i64> =
        sqlx::query_scalar("SELECT version FROM _sqlx_migrations ORDER BY version")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(migration_versions, (1_i64..=10).collect::<Vec<_>>());

    let now = Utc
        .with_ymd_and_hms(2026, 8, 13, 12, 0, 0)
        .single()
        .unwrap();
    let user_id = UserId::generate();
    let contexts = supporting_contexts(&verified);
    let currencies = contexts.currencies;
    let categories = contexts.categories;
    let preferences = contexts.preferences;
    let uah = currencies
        .require_enabled(CurrencyCode::new("UAH").unwrap())
        .await
        .unwrap();
    assert_eq!(uah.minor_unit, 2);
    let category = categories
        .create(
            CategoryCommand {
                user_id,
                name: "Foundation expense".to_owned(),
                kind: CategoryKind::Expense,
            },
            now,
        )
        .await
        .unwrap();
    assert_eq!(category.version, 1);
    let preference = preferences
        .set_base_currency(&currencies, user_id, uah.code, 0, now)
        .await
        .unwrap();
    assert!(preference.persisted);

    sqlx::query(
        "CREATE TABLE ledger.foundation_probe (\
            id UUID PRIMARY KEY, user_id UUID NOT NULL, description TEXT NOT NULL\
         )",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "CREATE TABLE reporting.foundation_effects (\
            effect_key TEXT PRIMARY KEY, executions INTEGER NOT NULL\
         )",
    )
    .execute(&pool)
    .await
    .unwrap();
    let probe_id = Uuid::new_v4();
    let event = representative_event(user_id, probe_id);

    let mut transaction = pool.begin().await.unwrap();
    sqlx::query(
        "INSERT INTO ledger.foundation_probe (id, user_id, description) VALUES ($1, $2, $3)",
    )
    .bind(probe_id)
    .bind(user_id.into_uuid())
    .bind("rolled back")
    .execute(&mut *transaction)
    .await
    .unwrap();
    PgOutboxWriter::from_transaction(&mut transaction)
        .append(&event)
        .await
        .unwrap();
    transaction.rollback().await.unwrap();
    let rolled_back: (i64, i64) = sqlx::query_as(
        "SELECT \
           (SELECT count(*) FROM ledger.foundation_probe), \
           (SELECT count(*) FROM integration.outbox_messages)",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(rolled_back, (0, 0));

    let mut transaction = pool.begin().await.unwrap();
    sqlx::query(
        "INSERT INTO ledger.foundation_probe (id, user_id, description) VALUES ($1, $2, $3)",
    )
    .bind(probe_id)
    .bind(user_id.into_uuid())
    .bind("committed")
    .execute(&mut *transaction)
    .await
    .unwrap();
    PgOutboxWriter::from_transaction(&mut transaction)
        .append(&event)
        .await
        .unwrap();
    transaction.commit().await.unwrap();

    let deliveries = Arc::new(Mutex::new(Vec::new()));
    let crashing = OutboxDispatcher::new(
        &verified,
        "foundation-crashing-publisher",
        RecordingPublisher {
            deliveries: Arc::clone(&deliveries),
            fail_after_publish: true,
        },
        dispatcher_config(),
    )
    .unwrap();
    assert_eq!(crashing.dispatch_batch().await.unwrap().retry_scheduled, 1);
    tokio::time::sleep(Duration::from_millis(5)).await;

    let replacement = OutboxDispatcher::new(
        &verified,
        "foundation-replacement-publisher",
        RecordingPublisher {
            deliveries: Arc::clone(&deliveries),
            fail_after_publish: false,
        },
        dispatcher_config(),
    )
    .unwrap();
    assert_eq!(replacement.dispatch_batch().await.unwrap().published, 1);
    assert_eq!(
        deliveries.lock().unwrap().as_slice(),
        &[event.envelope.event_id(); 2]
    );

    let consumer = ConsumerName::new("foundation-reporting-projection").unwrap();
    assert_eq!(
        consume_once(&pool, &consumer, &event).await,
        InboxOutcome::Applied
    );
    assert_eq!(
        consume_once(&pool, &consumer, &event).await,
        InboxOutcome::Duplicate
    );
    let executions: i32 = sqlx::query_scalar(
        "SELECT executions FROM reporting.foundation_effects WHERE effect_key = 'projection'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(executions, 1);

    let key = ProcessKey::new(
        "foundation-process",
        event.envelope.correlation_id().to_string(),
    )
    .unwrap();
    let state = ProcessState::new(
        key.clone(),
        json!({"event_id": event.envelope.event_id()}),
        ProcessStatus::new("pending").unwrap(),
    );
    let mut transaction = pool.begin().await.unwrap();
    let first = PgProcessManagerStore::from_transaction(&mut transaction)
        .acquire_lease(&key, "holder-a", Duration::from_millis(20))
        .await
        .unwrap();
    transaction.commit().await.unwrap();
    tokio::time::sleep(Duration::from_millis(35)).await;

    let mut transaction = pool.begin().await.unwrap();
    let second = PgProcessManagerStore::from_transaction(&mut transaction)
        .acquire_lease(&key, "holder-b", Duration::from_secs(1))
        .await
        .unwrap();
    transaction.commit().await.unwrap();
    assert!(second.fencing_token() > first.fencing_token());

    let mut transaction = pool.begin().await.unwrap();
    let stale = PgProcessManagerStore::from_transaction(&mut transaction)
        .save(&state, &first)
        .await
        .unwrap_err();
    assert!(matches!(stale, ProcessError::LeaseFenced));
    transaction.rollback().await.unwrap();

    let mut transaction = pool.begin().await.unwrap();
    PgProcessManagerStore::from_transaction(&mut transaction)
        .save(&state, &second)
        .await
        .unwrap();
    transaction.commit().await.unwrap();
}
