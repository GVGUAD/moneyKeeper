mod v2_test_support;

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{Duration, TimeZone, Utc};
use moneykeeper::{
    contexts::banking::{self, public::*},
    shared_kernel::{CorrelationId, CurrencyCode, IdempotencyKey, Money, UserId},
};
use rust_decimal_macros::dec;
use uuid::Uuid;

struct FixtureProvider;

#[async_trait]
impl ProviderClient for FixtureProvider {
    async fn client_info(
        &self,
        _credential: &ProviderCredential,
    ) -> Result<String, ProviderFailure> {
        Ok(r#"{"accounts":[{"id":"card-1","currencyCode":980,"balance":10000,"creditLimit":0,"maskedPan":["4444******1111"],"type":"black","iban":""},{"id":"card-2","currencyCode":980,"balance":20000,"creditLimit":0,"maskedPan":["4444******2222"],"type":"black","iban":""}],"jars":[]}"#.to_owned())
    }
}

async fn banking_fixture() -> (
    BankingFacade,
    sqlx::PgPool,
    UserId,
    ProviderConnectionId,
    Vec<ExternalResourceId>,
) {
    let (verified, pool) = v2_test_support::fresh_v2_runtime().await;
    let supporting = moneykeeper::bootstrap::v2::supporting_contexts(&verified);
    let banking = banking::build_with_ledger(
        &verified,
        Arc::new(Aes256CredentialCipher::new("test-key", [6_u8; 32]).unwrap()),
        Arc::new(FixtureProvider),
        supporting.ledger,
        supporting.currencies,
        [1_u8; 32],
    );
    let user_id = UserId::new(Uuid::new_v4());
    let connection = banking
        .connect_provider(ConnectProvider {
            user_id,
            provider: "monobank".to_owned(),
            credential: ProviderCredential::new("token").unwrap(),
            idempotency_key: IdempotencyKey::new("connect-sync").unwrap(),
            correlation_id: CorrelationId::generate(),
            requested_at: Utc::now(),
        })
        .await
        .unwrap()
        .connection;
    banking
        .validate_and_discover(user_id, connection.id)
        .await
        .unwrap();
    let ids: Vec<Uuid> = sqlx::query_scalar(
        "SELECT id FROM banking.external_resources WHERE user_id=$1 ORDER BY external_resource_id",
    )
    .bind(user_id.into_uuid())
    .fetch_all(&pool)
    .await
    .unwrap();
    (
        banking,
        pool,
        user_id,
        connection.id,
        ids.into_iter().map(ExternalResourceId::new).collect(),
    )
}

fn intake(
    user_id: UserId,
    connection_id: ProviderConnectionId,
    resource_id: ExternalResourceId,
    revision: i64,
    amount: rust_decimal::Decimal,
) -> IntakeProviderEvent {
    let now = Utc.with_ymd_and_hms(2026, 8, 20, 12, 0, 0).unwrap();
    IntakeProviderEvent {
        user_id,
        connection_id,
        resource_id,
        external_event_id: "statement-event".to_owned(),
        revision,
        state: if revision == 1 {
            ProviderTransactionState::Pending
        } else {
            ProviderTransactionState::Settled
        },
        operation_money: Money::new(amount, CurrencyCode::new("UAH").unwrap(), 2).unwrap(),
        description: "sanitized merchant".to_owned(),
        effective_at: now,
        recorded_at: now,
        correlation_id: CorrelationId::generate(),
    }
}

#[tokio::test]
async fn duplicate_revision_and_conflicting_content_are_distinguished_durably() {
    let (banking, pool, user, connection, resources) = banking_fixture().await;
    let first = banking
        .intake_provider_event(intake(user, connection, resources[0], 1, dec!(-10.00)))
        .await
        .unwrap();
    assert_eq!(first.outcome, ProviderEventIntakeOutcome::New);
    let duplicate = banking
        .intake_provider_event(intake(user, connection, resources[0], 1, dec!(-10.00)))
        .await
        .unwrap();
    assert_eq!(duplicate.provider_event_id, first.provider_event_id);
    assert_eq!(duplicate.outcome, ProviderEventIntakeOutcome::Duplicate);
    let conflict = banking
        .intake_provider_event(intake(user, connection, resources[0], 1, dec!(-12.00)))
        .await
        .unwrap();
    assert_eq!(
        conflict.outcome,
        ProviderEventIntakeOutcome::ConflictingContent
    );
    let count:i64=sqlx::query_scalar("SELECT count(*) FROM banking.provider_events WHERE external_resource_id=$1 AND external_event_id='statement-event'").bind(resources[0].into_uuid()).fetch_one(&pool).await.unwrap();
    assert_eq!(count, 1);
    let state: String = sqlx::query_scalar(
        "SELECT state FROM banking.provider_event_processes WHERE provider_event_id=$1",
    )
    .bind(first.provider_event_id.into_uuid())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(state, "quarantined");
}

#[tokio::test]
async fn revision_identity_is_scoped_to_resource_and_new_facts_publish_ready_events() {
    let (banking, pool, user, connection, resources) = banking_fixture().await;
    let first = banking
        .intake_provider_event(intake(user, connection, resources[0], 1, dec!(-10.00)))
        .await
        .unwrap();
    let other = banking
        .intake_provider_event(intake(user, connection, resources[1], 1, dec!(-10.00)))
        .await
        .unwrap();
    let settled = banking
        .intake_provider_event(intake(user, connection, resources[0], 2, dec!(-10.00)))
        .await
        .unwrap();
    assert_ne!(first.provider_event_id, other.provider_event_id);
    assert_ne!(first.provider_event_id, settled.provider_event_id);
    let outbox:i64=sqlx::query_scalar("SELECT count(*) FROM integration.outbox_messages WHERE event_type='banking.provider-event-ready.v1' AND user_id=$1").bind(user.into_uuid()).fetch_one(&pool).await.unwrap();
    assert_eq!(outbox, 3);
}

#[tokio::test]
async fn sync_claims_are_connection_scoped_fenced_and_advance_only_complete_pages() {
    let (banking, _pool, user, connection, _resources) = banking_fixture().await;
    let now = Utc.with_ymd_and_hms(2026, 8, 20, 10, 0, 0).unwrap();
    let job = banking
        .request_sync_job(RequestSyncJob {
            user_id: user,
            connection_id: connection,
            requested_from: now - Duration::days(1),
            requested_to: now,
            overlap_seconds: 3600,
            idempotency_key: IdempotencyKey::new("sync-1").unwrap(),
            correlation_id: CorrelationId::generate(),
        })
        .await
        .unwrap();
    let (first, second) = tokio::join!(
        banking.claim_due_sync_job("worker-a", now, 60),
        banking.claim_due_sync_job("worker-b", now, 60)
    );
    let first = first
        .unwrap()
        .or(second.unwrap())
        .expect("one worker claims the job");
    assert_eq!(first.id, job.id);
    let page = banking
        .begin_sync_page(BeginSyncPage {
            user_id: user,
            sync_job_id: job.id,
            holder: first.lease_holder.clone().unwrap(),
            fencing_token: first.fencing_token,
            provider_cursor: None,
            next_cursor: None,
            expected_events: 2,
            now: now + Duration::seconds(1),
        })
        .await
        .unwrap();
    let incomplete = banking
        .complete_sync_page(CompleteSyncPage {
            user_id: user,
            sync_job_id: job.id,
            sync_page_id: page.id,
            holder: first.lease_holder.clone().unwrap(),
            fencing_token: first.fencing_token,
            processed_events: 1,
            quarantined_events: 0,
            now: now + Duration::seconds(2),
        })
        .await;
    assert!(incomplete.is_err());
    let current = banking
        .claim_due_sync_job("worker-c", now + Duration::seconds(61), 60)
        .await
        .unwrap()
        .unwrap();
    assert!(current.fencing_token > first.fencing_token);
    let stale = banking
        .complete_sync_page(CompleteSyncPage {
            user_id: user,
            sync_job_id: job.id,
            sync_page_id: page.id,
            holder: first.lease_holder.unwrap(),
            fencing_token: first.fencing_token,
            processed_events: 1,
            quarantined_events: 1,
            now: now + Duration::seconds(62),
        })
        .await;
    assert!(stale.is_err());
    let completed = banking
        .complete_sync_page(CompleteSyncPage {
            user_id: user,
            sync_job_id: job.id,
            sync_page_id: page.id,
            holder: current.lease_holder.unwrap(),
            fencing_token: current.fencing_token,
            processed_events: 1,
            quarantined_events: 1,
            now: now + Duration::seconds(62),
        })
        .await
        .unwrap();
    assert_eq!(completed.state, "completed");
}
