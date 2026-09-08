#[path = "test_support.rs"]
mod test_support;

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::{Duration, Utc};
use moneykeeper::{
    contexts::{
        banking::{
            self,
            adapters::Aes256CredentialCipher,
            public::{
                BindExistingResource, ConnectProvider, ConnectionState, ProviderClient,
                ProviderCredential, ProviderFailure, RequestSyncJob,
            },
        },
        ledger::public::{AccountKind, AccountNature, OpenAccount},
    },
    shared_kernel::{CorrelationId, CurrencyCode, IdempotencyKey, Money, UserId},
};
use rust_decimal::Decimal;

#[derive(Default)]
struct WorkerProvider {
    callback: Mutex<Option<String>>,
    statement_accounts: Mutex<Vec<String>>,
}

#[async_trait]
impl ProviderClient for WorkerProvider {
    async fn client_info(
        &self,
        _credential: &ProviderCredential,
    ) -> Result<String, ProviderFailure> {
        Ok(r#"{"accounts":[{"id":"card-worker","currencyCode":980,"balance":10000,"creditLimit":0,"maskedPan":["4444******1111"],"type":"black","iban":""},{"id":"card-other","currencyCode":980,"balance":20000,"creditLimit":0,"maskedPan":["4444******2222"],"type":"black","iban":""}],"jars":[]}"#.to_owned())
    }

    async fn register_webhook(
        &self,
        _credential: &ProviderCredential,
        callback_url: &str,
    ) -> Result<(), ProviderFailure> {
        *self.callback.lock().unwrap() = Some(callback_url.to_owned());
        Ok(())
    }

    async fn statement(
        &self,
        _credential: &ProviderCredential,
        account: &str,
        from: chrono::DateTime<Utc>,
        _to: chrono::DateTime<Utc>,
    ) -> Result<String, ProviderFailure> {
        self.statement_accounts
            .lock()
            .unwrap()
            .push(account.to_owned());
        Ok(format!(
            r#"[{{"id":"worker-event-{account}","time":{},"description":"Worker purchase","mcc":5411,"hold":true,"amount":-1000,"operationAmount":-2500,"currencyCode":643,"balance":9000}}]"#,
            from.timestamp(),
        ))
    }
}

#[tokio::test]
async fn pending_connection_activates_registers_and_fetches_a_snapshot_window() {
    let (verified, pool) = test_support::fresh_runtime().await;
    let contexts = moneykeeper::bootstrap::build_contexts(&verified);
    let ledger = contexts.ledger.clone();
    let provider = Arc::new(WorkerProvider::default());
    let banking = banking::build_with_ledger(
        &verified,
        Arc::new(Aes256CredentialCipher::new("worker-key", [0x44; 32]).unwrap()),
        provider.clone(),
        contexts.ledger,
        contexts.currencies,
        [0x55; 32],
    );
    let user_id = UserId::generate();
    let now = Utc::now();
    let connection = banking
        .connect_provider(ConnectProvider {
            user_id,
            provider: "monobank".to_owned(),
            credential: ProviderCredential::new("worker-token").unwrap(),
            idempotency_key: IdempotencyKey::new("worker-connect").unwrap(),
            correlation_id: CorrelationId::generate(),
            requested_at: now,
        })
        .await
        .unwrap()
        .connection;
    assert_eq!(connection.state, ConnectionState::Pending);

    let validation = banking
        .run_validation_once("worker-validation", now + Duration::seconds(1))
        .await
        .unwrap();
    assert!(validation.claimed);
    let active = banking
        .get_connection(user_id, connection.id)
        .await
        .unwrap();
    assert_eq!(active.state, ConnectionState::Active);
    assert_eq!(active.validation_state, "succeeded");
    assert_eq!(active.webhook_registration_state, "pending");
    let resources = banking
        .list_resources(user_id, connection.id)
        .await
        .unwrap();
    let target = resources
        .iter()
        .find(|resource| resource.provider_resource_id == "card-worker")
        .unwrap();
    assert_eq!(resources.len(), 2);

    let currency = CurrencyCode::new("UAH").unwrap();
    let account = ledger
        .open_account(OpenAccount {
            user_id,
            name: "Worker card".to_owned(),
            currency: currency.clone(),
            kind: AccountKind::DebitCard,
            nature: AccountNature::Asset,
            opening_balance: Money::new(Decimal::ZERO, currency, 2).unwrap(),
            idempotency_key: IdempotencyKey::new("worker-open-account").unwrap(),
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
            resource_id: target.id,
            ledger_account_id: account.id,
            expected_resource_version: target.version,
            idempotency_key: IdempotencyKey::new("worker-map-resource").unwrap(),
            correlation_id: CorrelationId::generate(),
            requested_at: now,
        })
        .await
        .unwrap();

    let registration = banking
        .run_webhook_registration_once(
            "worker-registration",
            "https://callback.invalid/",
            now + Duration::seconds(2),
        )
        .await
        .unwrap();
    assert!(registration.claimed);
    assert_eq!(
        banking
            .get_connection(user_id, connection.id)
            .await
            .unwrap()
            .webhook_registration_state,
        "registered"
    );
    let callback = provider.callback.lock().unwrap().clone().unwrap();
    assert!(callback.starts_with("https://callback.invalid/webhooks/monobank/"));

    let job = banking
        .request_sync_job(RequestSyncJob {
            user_id,
            connection_id: connection.id,
            resource_id: target.id,
            requested_from: now - Duration::days(31),
            requested_to: now,
            overlap_seconds: 0,
            idempotency_key: IdempotencyKey::new("worker-sync").unwrap(),
            correlation_id: CorrelationId::generate(),
        })
        .await
        .unwrap();
    let statement = banking
        .run_statement_once("worker-statement", now + Duration::seconds(3))
        .await
        .unwrap();
    assert!(statement.claimed);
    assert_eq!(statement.records, 1);
    assert_eq!(
        *provider.statement_accounts.lock().unwrap(),
        vec!["card-worker"]
    );
    let pages = banking.list_sync_pages(user_id, job.id).await.unwrap();
    assert_eq!(pages.len(), 1);
    assert_eq!(pages[0].state, "waiting_for_events");
    assert_eq!(pages[0].expected_events, 1);
    let original_currency: Option<String> = sqlx::query_scalar(
        "SELECT original_currency FROM banking.provider_events \
         WHERE external_event_id='worker-event-card-worker' AND user_id=$1",
    )
    .bind(user_id.into_uuid())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(original_currency.as_deref(), Some("RUB"));
    let windows: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM banking.sync_job_resources WHERE sync_job_id=$1 AND user_id=$2",
    )
    .bind(job.id.into_uuid())
    .bind(user_id.into_uuid())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(windows, 1);
}
