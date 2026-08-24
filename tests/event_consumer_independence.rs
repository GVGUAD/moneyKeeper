mod test_support;

use chrono::Utc;
use moneykeeper::bootstrap::event_consumers;
use moneykeeper::contexts::reference_data::public::FX_OBSERVED_V1;
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

const RECURRING_RECEIPT: &str = "recurring-event-policy-v1";
const REPORTING_RECEIPT: &str = "reporting-projections-v1";

#[tokio::test]
async fn irrelevant_events_advance_each_consumer_independently() {
    let (database, pool) = test_support::fresh_runtime().await;
    let event_id = insert_event(&pool, "other-context.fact.v1", 1, json!({})).await;
    let consumers = event_consumers(&database);

    let recurring = consumers.run_recurring_once().await.unwrap();
    assert!(recurring.claimed);
    assert_eq!(recurring.records, 0);
    assert!(has_receipt(&pool, RECURRING_RECEIPT, event_id).await);
    assert!(!has_receipt(&pool, REPORTING_RECEIPT, event_id).await);

    let reporting = consumers.run_reporting_once().await.unwrap();
    assert!(reporting.claimed);
    assert_eq!(reporting.records, 0);
    assert!(has_receipt(&pool, REPORTING_RECEIPT, event_id).await);
}

#[tokio::test]
async fn unsupported_version_blocks_only_the_interested_consumer() {
    let (database, pool) = test_support::fresh_runtime().await;
    let event_id = insert_event(&pool, FX_OBSERVED_V1, 2, valid_fx_payload()).await;
    let consumers = event_consumers(&database);

    assert!(consumers.run_recurring_once().await.unwrap().claimed);
    assert!(has_receipt(&pool, RECURRING_RECEIPT, event_id).await);

    assert!(consumers.run_reporting_once().await.is_err());
    assert!(consumers.run_reporting_once().await.is_err());
    assert!(!has_receipt(&pool, REPORTING_RECEIPT, event_id).await);
}

#[tokio::test]
async fn malformed_known_event_remains_retryable_without_a_receipt() {
    let (database, pool) = test_support::fresh_runtime().await;
    let event_id = insert_event(&pool, FX_OBSERVED_V1, 1, json!({})).await;
    let consumers = event_consumers(&database);

    assert!(consumers.run_recurring_once().await.unwrap().claimed);
    assert!(consumers.run_reporting_once().await.is_err());
    assert!(consumers.run_reporting_once().await.is_err());
    assert!(!has_receipt(&pool, REPORTING_RECEIPT, event_id).await);
}

#[tokio::test]
async fn failed_projection_rolls_back_effect_and_feed_receipts() {
    let (database, pool) = test_support::fresh_runtime().await;
    let event_id = insert_event(&pool, FX_OBSERVED_V1, 1, valid_fx_payload()).await;
    sqlx::query("DROP TABLE reporting.fx_rates")
        .execute(&pool)
        .await
        .unwrap();
    let consumers = event_consumers(&database);

    assert!(consumers.run_reporting_once().await.is_err());
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM reporting.consumed_events")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
    assert!(!has_receipt(&pool, REPORTING_RECEIPT, event_id).await);
}

#[tokio::test]
async fn retry_after_lost_feed_receipt_deduplicates_the_projection_effect() {
    let (database, pool) = test_support::fresh_runtime().await;
    let event_id = insert_event(&pool, FX_OBSERVED_V1, 1, valid_fx_payload()).await;
    let consumers = event_consumers(&database);

    assert_eq!(consumers.run_reporting_once().await.unwrap().records, 1);
    sqlx::query("DELETE FROM integration.inbox_receipts WHERE consumer_name=$1 AND message_id=$2")
        .bind(REPORTING_RECEIPT)
        .bind(event_id)
        .execute(&pool)
        .await
        .unwrap();

    let retry = consumers.run_reporting_once().await.unwrap();
    assert!(retry.claimed);
    assert_eq!(retry.records, 1, "the known event was handled idempotently");
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM reporting.fx_rates")
            .fetch_one(&pool)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM reporting.consumed_events")
            .fetch_one(&pool)
            .await
            .unwrap(),
        1
    );
    assert!(has_receipt(&pool, REPORTING_RECEIPT, event_id).await);
}

async fn insert_event(
    pool: &PgPool,
    event_type: &str,
    schema_version: i32,
    payload: Value,
) -> Uuid {
    let event_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO integration.outbox_messages \
         (message_id,event_id,message_schema_version,context_name,aggregate_id,aggregate_version, \
          event_type,user_id,occurred_at,correlation_id,payload) \
         VALUES($1,$2,$3,'test','test-aggregate',1,$4,$5,$6,$7,$8)",
    )
    .bind(Uuid::new_v4())
    .bind(event_id)
    .bind(schema_version)
    .bind(event_type)
    .bind(Uuid::new_v4())
    .bind(Utc::now())
    .bind(Uuid::new_v4())
    .bind(payload)
    .execute(pool)
    .await
    .unwrap();
    event_id
}

async fn has_receipt(pool: &PgPool, consumer: &str, event_id: Uuid) -> bool {
    sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM integration.inbox_receipts \
         WHERE consumer_name=$1 AND message_id=$2)",
    )
    .bind(consumer)
    .bind(event_id)
    .fetch_one(pool)
    .await
    .unwrap()
}

fn valid_fx_payload() -> Value {
    let now = Utc::now();
    let observation_id = Uuid::new_v4();
    json!({
        "observation_id": observation_id,
        "source": "test",
        "source_revision": "one",
        "base_currency": "USD",
        "quote_currency": "UAH",
        "rate": "40.000000000000",
        "effective_at": now,
        "observed_at": now,
        "recorded_at": now
    })
}
