mod v2_test_support;

use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Bytes;
use axum_test::TestServer;
use chrono::Utc;
use moneykeeper::{
    contexts::banking::{self, public::*},
    shared_kernel::{CorrelationId, IdempotencyKey, UserId},
};
use uuid::Uuid;

struct FixtureProvider;
#[async_trait]
impl ProviderClient for FixtureProvider {
    async fn client_info(&self, _: &ProviderCredential) -> Result<String, ProviderFailure> {
        Ok(r#"{"accounts":[{"id":"card-1","currencyCode":980,"balance":10000,"creditLimit":0,"maskedPan":["4444******1111"],"type":"black","iban":""}],"jars":[]}"#.to_owned())
    }
}

#[tokio::test]
async fn webhook_credentials_are_high_entropy_rotatable_and_queue_replays_once() {
    let (verified, pool) = v2_test_support::fresh_v2_runtime().await;
    let supporting = moneykeeper::bootstrap::v2::supporting_contexts(&verified);
    let banking = banking::build_with_ledger(
        &verified,
        Arc::new(Aes256CredentialCipher::new("test-key", [7_u8; 32]).unwrap()),
        Arc::new(FixtureProvider),
        supporting.ledger,
        supporting.currencies,
        [3_u8; 32],
    );
    let user = UserId::new(Uuid::new_v4());
    let connection = banking
        .connect_provider(ConnectProvider {
            user_id: user,
            provider: "monobank".to_owned(),
            credential: ProviderCredential::new("token").unwrap(),
            idempotency_key: IdempotencyKey::new("connect-webhook").unwrap(),
            correlation_id: CorrelationId::generate(),
            requested_at: Utc::now(),
        })
        .await
        .unwrap()
        .connection;
    banking
        .validate_and_discover(user, connection.id)
        .await
        .unwrap();
    let current = banking.list_connections(user).await.unwrap().pop().unwrap();
    let rotated = banking
        .rotate_webhook_credential(RotateWebhookCredential {
            user_id: user,
            connection_id: connection.id,
            expected_version: current.version,
            requested_at: Utc::now(),
        })
        .await
        .unwrap();
    let secret = rotated.credential.expose().to_owned();
    assert!(secret.len() >= 43);
    assert_eq!(
        format!("{:?}", rotated.credential),
        "WebhookCredential([REDACTED])"
    );
    let bytes: Vec<u8> = sqlx::query_scalar(
        "SELECT webhook_credential_ciphertext FROM banking.provider_connections WHERE id=$1",
    )
    .bind(connection.id.into_uuid())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(
        !bytes
            .windows(secret.len())
            .any(|window| window == secret.as_bytes())
    );
    banking
        .register_pending_webhook(user, connection.id, "https://callback.invalid")
        .await
        .unwrap();
    let registration:(String,i32)=sqlx::query_as("SELECT webhook_registration_state,webhook_registration_attempts FROM banking.provider_connections WHERE id=$1").bind(connection.id.into_uuid()).fetch_one(&pool).await.unwrap();
    assert_eq!(registration, ("retry_due".to_owned(), 1));
    let server = TestServer::new(banking::webhook_router(banking.clone())).unwrap();
    assert_eq!(
        server
            .get(&format!("/webhooks/monobank/{secret}"))
            .await
            .status_code(),
        200
    );
    assert_eq!(
        server.get("/webhooks/monobank/invalid").await.status_code(),
        404
    );
    assert_eq!(
        server
            .post(&format!("/webhooks/monobank/{secret}"))
            .bytes(Bytes::from_static(b"sanitized-notification"))
            .await
            .status_code(),
        200
    );
    assert_eq!(
        server
            .post(&format!("/webhooks/monobank/{secret}"))
            .bytes(Bytes::from_static(b"sanitized-notification"))
            .await
            .status_code(),
        200
    );
    let receipts: i64 =
        sqlx::query_scalar("SELECT count(*) FROM banking.webhook_receipts WHERE connection_id=$1")
            .bind(connection.id.into_uuid())
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(receipts, 1);
    let next = banking
        .rotate_webhook_credential(RotateWebhookCredential {
            user_id: user,
            connection_id: connection.id,
            expected_version: rotated.connection_version,
            requested_at: Utc::now(),
        })
        .await
        .unwrap();
    assert_ne!(next.credential.expose(), secret);
    assert_eq!(
        server
            .get(&format!("/webhooks/monobank/{secret}"))
            .await
            .status_code(),
        404
    );
}
