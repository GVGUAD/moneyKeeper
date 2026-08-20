mod v2_test_support;

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use async_trait::async_trait;
use axum::body::Bytes;
use axum_test::TestServer;
use chrono::{Duration, TimeZone, Utc};
use moneykeeper::{
    contexts::{
        banking::{self, public::*},
        ledger::public::{AccountKind, AccountNature, ApproveReconciliation, OpenAccount},
    },
    integration::process_managers::{
        banking_import::import_provider_revision, banking_observation::deliver_balance_observation,
    },
    shared_kernel::{CorrelationId, CurrencyCode, IdempotencyKey, Money, UserId},
};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

#[derive(Default)]
struct RestartableProvider {
    registration_attempts: AtomicUsize,
}

#[async_trait]
impl ProviderClient for RestartableProvider {
    async fn client_info(
        &self,
        _credential: &ProviderCredential,
    ) -> Result<String, ProviderFailure> {
        Ok(r#"{
          "accounts":[
            {"id":"card-1","currencyCode":980,"balance":10000,"creditLimit":0,"maskedPan":["4444******1111"],"type":"black","iban":""},
            {"id":"unknown-1","currencyCode":980,"balance":0,"creditLimit":0,"maskedPan":[],"type":"brokerage","iban":""}
          ],
          "jars":[{"id":"jar-1","title":"Reserve","currencyCode":980,"balance":5000}]
        }"#
        .to_owned())
    }

    async fn register_webhook(
        &self,
        _credential: &ProviderCredential,
        _callback_url: &str,
    ) -> Result<(), ProviderFailure> {
        if self.registration_attempts.fetch_add(1, Ordering::SeqCst) == 0 {
            Err(ProviderFailure::Classified {
                class: ProviderFailureClass::Transient,
            })
        } else {
            Ok(())
        }
    }
}

fn provider_event(
    user_id: UserId,
    connection_id: ProviderConnectionId,
    resource_id: ExternalResourceId,
    revision: i64,
    state: ProviderTransactionState,
    amount: Decimal,
) -> IntakeProviderEvent {
    let at = Utc.with_ymd_and_hms(2026, 8, 20, 12, 0, 0).unwrap() + Duration::seconds(revision);
    IntakeProviderEvent {
        user_id,
        connection_id,
        resource_id,
        external_event_id: "workflow-event".to_owned(),
        revision,
        state,
        operation_money: Money::new(amount, CurrencyCode::new("UAH").unwrap(), 2).unwrap(),
        description: "sanitized provider purchase".to_owned(),
        effective_at: at,
        recorded_at: at,
        correlation_id: CorrelationId::generate(),
    }
}

#[tokio::test]
async fn phase3_is_revision_safe_restartable_and_keeps_ledger_authoritative() {
    let (verified, pool) = v2_test_support::fresh_v2_runtime().await;
    let supporting = moneykeeper::bootstrap::v2::supporting_contexts(&verified);
    let ledger = supporting.ledger;
    let provider = Arc::new(RestartableProvider::default());
    let build_banking = || {
        banking::build_with_ledger(
            &verified,
            Arc::new(Aes256CredentialCipher::new("phase3-key", [0x33; 32]).unwrap()),
            provider.clone(),
            ledger.clone(),
            supporting.currencies.clone(),
            [0x66; 32],
        )
    };
    let banking = build_banking();
    let user_id = UserId::generate();
    let now = Utc::now();

    let connection = banking
        .connect_provider(ConnectProvider {
            user_id,
            provider: "monobank".to_owned(),
            credential: ProviderCredential::new("workflow-x-token").unwrap(),
            idempotency_key: IdempotencyKey::new("phase3-connect").unwrap(),
            correlation_id: CorrelationId::generate(),
            requested_at: now,
        })
        .await
        .unwrap()
        .connection;
    let ciphertext: Vec<u8> = sqlx::query_scalar(
        "SELECT active_credential_ciphertext FROM banking.provider_connections WHERE id=$1",
    )
    .bind(connection.id.into_uuid())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(
        !ciphertext
            .windows("workflow-x-token".len())
            .any(|window| window == b"workflow-x-token")
    );

    let discovered = banking
        .validate_and_discover(user_id, connection.id)
        .await
        .unwrap();
    assert_eq!(discovered.len(), 3);
    let resources = banking
        .list_resources(user_id, connection.id)
        .await
        .unwrap();
    let card = resources
        .iter()
        .find(|resource| resource.kind == ResourceKind::Card)
        .unwrap();
    let jar = resources
        .iter()
        .find(|resource| resource.kind == ResourceKind::Jar)
        .unwrap();
    let unsupported = resources
        .iter()
        .find(|resource| resource.kind == ResourceKind::Unsupported)
        .unwrap();
    assert_eq!(unsupported.discovery_state, "unsupported");

    let account = ledger
        .open_account(OpenAccount {
            user_id,
            name: "Workflow card".to_owned(),
            currency: CurrencyCode::new("UAH").unwrap(),
            kind: AccountKind::DebitCard,
            nature: AccountNature::Asset,
            opening_balance: Money::new(Decimal::ZERO, CurrencyCode::new("UAH").unwrap(), 2)
                .unwrap(),
            idempotency_key: IdempotencyKey::new("phase3-open-card").unwrap(),
            correlation_id: CorrelationId::generate(),
            causation_id: None,
            occurred_at: now,
        })
        .await
        .unwrap()
        .account;
    banking
        .bind_existing_resource(BindExistingResource {
            user_id,
            resource_id: card.id,
            ledger_account_id: account.id,
            expected_resource_version: card.version,
            idempotency_key: IdempotencyKey::new("phase3-bind-card").unwrap(),
            correlation_id: CorrelationId::generate(),
            requested_at: now,
        })
        .await
        .unwrap();
    let jar_mapping = banking
        .create_and_map_resource(CreateAndMapResource {
            user_id,
            resource_id: jar.id,
            account_name: "Workflow reserve".to_owned(),
            expected_resource_version: jar.version,
            idempotency_key: IdempotencyKey::new("phase3-map-jar").unwrap(),
            correlation_id: CorrelationId::generate(),
            requested_at: now,
        })
        .await
        .unwrap();
    let replayed_jar = banking
        .create_and_map_resource(CreateAndMapResource {
            user_id,
            resource_id: jar.id,
            account_name: "Workflow reserve".to_owned(),
            expected_resource_version: jar.version,
            idempotency_key: IdempotencyKey::new("phase3-map-jar").unwrap(),
            correlation_id: CorrelationId::generate(),
            requested_at: now,
        })
        .await
        .unwrap();
    assert_eq!(
        jar_mapping.mapping.ledger_account_id,
        replayed_jar.mapping.ledger_account_id
    );
    let unmappable = banking
        .create_and_map_resource(CreateAndMapResource {
            user_id,
            resource_id: unsupported.id,
            account_name: "Must not be created".to_owned(),
            expected_resource_version: unsupported.version,
            idempotency_key: IdempotencyKey::new("phase3-map-unsupported").unwrap(),
            correlation_id: CorrelationId::generate(),
            requested_at: now,
        })
        .await;
    assert!(unmappable.is_err());

    let sync = banking
        .request_sync_job(RequestSyncJob {
            user_id,
            connection_id: connection.id,
            requested_from: now - Duration::days(1),
            requested_to: now,
            overlap_seconds: 3_600,
            idempotency_key: IdempotencyKey::new("phase3-sync").unwrap(),
            correlation_id: CorrelationId::generate(),
        })
        .await
        .unwrap();
    let sync_replay = banking
        .request_sync_job(RequestSyncJob {
            user_id,
            connection_id: connection.id,
            requested_from: now - Duration::days(1),
            requested_to: now,
            overlap_seconds: 3_600,
            idempotency_key: IdempotencyKey::new("phase3-sync").unwrap(),
            correlation_id: CorrelationId::generate(),
        })
        .await
        .unwrap();
    assert_eq!(sync_replay.id, sync.id);
    assert_eq!(sync_replay.requested_from, now - Duration::days(1));
    let sync_conflict = banking
        .request_sync_job(RequestSyncJob {
            user_id,
            connection_id: connection.id,
            requested_from: now - Duration::days(1),
            requested_to: now,
            overlap_seconds: 60,
            idempotency_key: IdempotencyKey::new("phase3-sync").unwrap(),
            correlation_id: CorrelationId::generate(),
        })
        .await;
    assert!(matches!(
        sync_conflict,
        Err(BankingError::IdempotencyConflict)
    ));
    let (claim_a, claim_b) = tokio::join!(
        banking.claim_due_sync_job("phase3-worker-a", now, 60),
        banking.claim_due_sync_job("phase3-worker-b", now, 60),
    );
    let claim = claim_a
        .unwrap()
        .or(claim_b.unwrap())
        .expect("one concurrent worker claims the sync");
    let page = banking
        .begin_sync_page(BeginSyncPage {
            user_id,
            sync_job_id: sync.id,
            holder: claim.lease_holder.clone().unwrap(),
            fencing_token: claim.fencing_token,
            provider_cursor: None,
            next_cursor: None,
            expected_events: 5,
            now: now + Duration::seconds(1),
        })
        .await
        .unwrap();

    let pending = banking
        .intake_provider_event(provider_event(
            user_id,
            connection.id,
            card.id,
            1,
            ProviderTransactionState::Pending,
            dec!(-10.00),
        ))
        .await
        .unwrap();
    let duplicate = banking
        .intake_provider_event(provider_event(
            user_id,
            connection.id,
            card.id,
            1,
            ProviderTransactionState::Pending,
            dec!(-10.00),
        ))
        .await
        .unwrap();
    assert_eq!(duplicate.outcome, ProviderEventIntakeOutcome::Duplicate);
    let settled = banking
        .intake_provider_event(provider_event(
            user_id,
            connection.id,
            card.id,
            2,
            ProviderTransactionState::Settled,
            dec!(-10.00),
        ))
        .await
        .unwrap();
    let corrected = banking
        .intake_provider_event(provider_event(
            user_id,
            connection.id,
            card.id,
            3,
            ProviderTransactionState::Settled,
            dec!(-12.00),
        ))
        .await
        .unwrap();
    let reversed = banking
        .intake_provider_event(provider_event(
            user_id,
            connection.id,
            card.id,
            4,
            ProviderTransactionState::Reversed,
            dec!(-12.00),
        ))
        .await
        .unwrap();

    for event_id in [
        pending.provider_event_id,
        settled.provider_event_id,
        corrected.provider_event_id,
        reversed.provider_event_id,
    ] {
        import_provider_revision(&banking, &ledger, user_id, event_id)
            .await
            .unwrap();
    }
    let restarted = build_banking();
    let replay =
        import_provider_revision(&restarted, &ledger, user_id, corrected.provider_event_id)
            .await
            .unwrap();
    assert!(replay.replayed);
    assert_eq!(
        ledger
            .list_journals(user_id, None, 100)
            .await
            .unwrap()
            .len(),
        4
    );

    let completed = restarted
        .complete_sync_page(CompleteSyncPage {
            user_id,
            sync_job_id: sync.id,
            sync_page_id: page.id,
            holder: claim.lease_holder.unwrap(),
            fencing_token: claim.fencing_token,
            processed_events: 4,
            quarantined_events: 1,
            now: now + Duration::seconds(2),
        })
        .await
        .unwrap();
    assert_eq!(completed.state, "completed");

    let journals_before_observation = ledger
        .list_journals(user_id, None, 100)
        .await
        .unwrap()
        .len();
    let observation = restarted
        .record_balance_observation(RecordBalanceObservation {
            user_id,
            connection_id: connection.id,
            resource_id: card.id,
            basis: BalanceBasis::Reported,
            provider_money: Money::new(dec!(100.00), CurrencyCode::new("UAH").unwrap(), 2).unwrap(),
            sign_semantics: "provider_native".to_owned(),
            comparability: BalanceComparability::Comparable(
                Money::new(dec!(100.00), CurrencyCode::new("UAH").unwrap(), 2).unwrap(),
            ),
            observed_at: now + Duration::minutes(1),
            recorded_at: now + Duration::minutes(1),
            correlation_id: CorrelationId::generate(),
        })
        .await
        .unwrap();
    let delivered = deliver_balance_observation(&restarted, &ledger, user_id, observation.id)
        .await
        .unwrap();
    assert_eq!(delivered.state, "delivered");
    assert_eq!(
        ledger
            .list_journals(user_id, None, 100)
            .await
            .unwrap()
            .len(),
        journals_before_observation
    );
    let case = ledger
        .get_reconciliation(user_id, delivered.reconciliation_case_id.unwrap())
        .await
        .unwrap();
    let approved = ledger
        .approve_reconciliation(ApproveReconciliation {
            user_id,
            case_id: case.id,
            expected_version: case.version,
            expected_balance_version: case.captured_balance_version,
            reason: "verified against provider statement".to_owned(),
            idempotency_key: IdempotencyKey::new("phase3-approve").unwrap(),
            correlation_id: CorrelationId::generate(),
            causation_id: None,
            occurred_at: now + Duration::minutes(2),
        })
        .await
        .unwrap();
    assert!(approved.journal_entry_id.is_some());

    let current = restarted
        .get_connection(user_id, connection.id)
        .await
        .unwrap();
    let rotation = restarted
        .rotate_webhook_credential(RotateWebhookCredential {
            user_id,
            connection_id: connection.id,
            expected_version: current.version,
            requested_at: now + Duration::minutes(3),
        })
        .await
        .unwrap();
    let webhook_credential = rotation.credential.expose().to_owned();
    restarted
        .register_pending_webhook(user_id, connection.id, "https://callback.invalid")
        .await
        .unwrap();
    restarted
        .register_pending_webhook(user_id, connection.id, "https://callback.invalid")
        .await
        .unwrap();
    let registration: String = sqlx::query_scalar(
        "SELECT webhook_registration_state FROM banking.provider_connections WHERE id=$1",
    )
    .bind(connection.id.into_uuid())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(registration, "registered");
    let callbacks = TestServer::new(banking::webhook_router(restarted.clone())).unwrap();
    assert_eq!(
        callbacks
            .post(&format!("/webhooks/monobank/{webhook_credential}"))
            .bytes(Bytes::from_static(b"phase3-notification"))
            .await
            .status_code(),
        200
    );
    assert_eq!(
        callbacks
            .post(&format!("/webhooks/monobank/{webhook_credential}"))
            .bytes(Bytes::from_static(b"phase3-notification"))
            .await
            .status_code(),
        200
    );
    let receipt_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM banking.webhook_receipts WHERE connection_id=$1")
            .bind(connection.id.into_uuid())
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(receipt_count, 1);

    let disconnected = restarted
        .disconnect(
            user_id,
            connection.id,
            rotation.connection_version,
            now + Duration::minutes(4),
        )
        .await
        .unwrap();
    assert_eq!(disconnected.state, ConnectionState::Revoked);
    assert_eq!(
        callbacks
            .get(&format!("/webhooks/monobank/{webhook_credential}"))
            .await
            .status_code(),
        404
    );
    let credentials_removed: bool = sqlx::query_scalar(
        "SELECT active_credential_ciphertext IS NULL
             AND pending_credential_ciphertext IS NULL
             AND webhook_credential_ciphertext IS NULL
         FROM banking.provider_connections WHERE id=$1",
    )
    .bind(connection.id.into_uuid())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(credentials_removed);
    assert_eq!(
        restarted
            .list_resources(user_id, connection.id)
            .await
            .unwrap()
            .len(),
        3
    );
}
