mod v2_test_support;

use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use moneykeeper::contexts::banking::{self, public::{
    Aes256CredentialCipher, ConnectProvider, ProviderClient, ProviderCredential, ProviderFailure,
}};
use moneykeeper::shared_kernel::{CorrelationId, IdempotencyKey, UserId};
use sqlx::Row;
use uuid::Uuid;

struct UnusedProvider;

#[async_trait]
impl ProviderClient for UnusedProvider {
    async fn client_info(&self, _credential: &ProviderCredential) -> Result<String, ProviderFailure> {
        panic!("provider must not be called while persisting the connection")
    }
}

struct FixtureProvider;

#[async_trait]
impl ProviderClient for FixtureProvider {
    async fn client_info(&self, _credential: &ProviderCredential) -> Result<String, ProviderFailure> {
        Ok(r#"{"accounts":[{"id":"card-1","currencyCode":980,"balance":10000,"creditLimit":0,"maskedPan":["4444******1111"],"type":"black","iban":""}],"jars":[{"id":"jar-1","title":"Reserve","currencyCode":980,"balance":5000}]}"#.to_owned())
    }
}

#[tokio::test]
async fn credential_validation_discovers_distinct_resources_and_activates_connection() {
    let (verified, pool) = v2_test_support::fresh_v2_runtime().await;
    let currencies = moneykeeper::bootstrap::v2::supporting_contexts(&verified).currencies;
    let facade = banking::build_with_adapters(
        &verified,
        Arc::new(Aes256CredentialCipher::new("test-key", [4_u8; 32]).unwrap()),
        Arc::new(FixtureProvider),
        currencies,
        [1_u8;32],
    );
    let user_id = UserId::new(Uuid::new_v4());
    let connection = facade.connect_provider(ConnectProvider {
        user_id,
        provider: "monobank".to_owned(),
        credential: ProviderCredential::new("sanitized-token").unwrap(),
        idempotency_key: IdempotencyKey::new("connect-discover").unwrap(),
        correlation_id: CorrelationId::generate(),
        requested_at: Utc::now(),
    }).await.unwrap().connection;

    let resources = facade.validate_and_discover(user_id, connection.id).await.unwrap();
    assert_eq!(resources.len(), 2);
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT external_resource_id,kind FROM banking.external_resources \
         WHERE user_id=$1 AND connection_id=$2 ORDER BY external_resource_id",
    )
    .bind(user_id.into_uuid())
    .bind(connection.id.into_uuid())
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(rows, vec![("card-1".to_owned(), "card".to_owned()), ("jar-1".to_owned(), "jar".to_owned())]);
    let state: String = sqlx::query_scalar(
        "SELECT state FROM banking.provider_connections WHERE id=$1 AND user_id=$2",
    )
    .bind(connection.id.into_uuid())
    .bind(user_id.into_uuid())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(state, "active");
}

#[tokio::test]
async fn credential_is_encrypted_before_connection_commit_and_never_returned() {
    let (verified, pool) = v2_test_support::fresh_v2_runtime().await;
    let currencies = moneykeeper::bootstrap::v2::supporting_contexts(&verified).currencies;
    let facade = banking::build_with_adapters(
        &verified,
        Arc::new(Aes256CredentialCipher::new("test-key", [9_u8; 32]).unwrap()),
        Arc::new(UnusedProvider),
        currencies,
        [1_u8;32],
    );
    let user_id = UserId::new(Uuid::new_v4());
    let token = "sanitized-x-token-that-must-not-appear";
    let result = facade
        .connect_provider(ConnectProvider {
            user_id,
            provider: "monobank".to_owned(),
            credential: ProviderCredential::new(token).unwrap(),
            idempotency_key: IdempotencyKey::new("connect-1").unwrap(),
            correlation_id: CorrelationId::generate(),
            requested_at: Utc::now(),
        })
        .await
        .unwrap();
    let row = sqlx::query(
        "SELECT active_credential_ciphertext, active_credential_nonce, active_credential_key_id \
         FROM banking.provider_connections WHERE id=$1 AND user_id=$2",
    )
    .bind(result.connection.id.into_uuid())
    .bind(user_id.into_uuid())
    .fetch_one(&pool)
    .await
    .unwrap();
    let ciphertext: Vec<u8> = row.get("active_credential_ciphertext");
    assert!(!ciphertext.windows(token.len()).any(|bytes| bytes == token.as_bytes()));
    assert_ne!(row.get::<Vec<u8>, _>("active_credential_nonce"), Vec::<u8>::new());
    assert_eq!(row.get::<String, _>("active_credential_key_id"), "test-key");
    assert!(!serde_json::to_string(&result).unwrap().contains(token));
}

#[tokio::test]
async fn schema_creates_banking_owned_tables_and_worker_indexes() {
    let (_verified, pool) = v2_test_support::fresh_v2_runtime().await;
    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT table_name FROM information_schema.tables \
         WHERE table_schema = 'banking' ORDER BY table_name",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    for table in [
        "balance_observations",
        "command_receipts",
        "external_resources",
        "provider_connections",
        "provider_events",
        "resource_mappings",
        "sync_jobs",
        "sync_pages",
        "webhook_receipts",
    ] {
        assert!(tables.iter().any(|actual| actual == table), "missing {table}");
    }

    let indexes: Vec<String> = sqlx::query_scalar(
        "SELECT indexname FROM pg_indexes WHERE schemaname = 'banking' ORDER BY indexname",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    for index in [
        "banking_connections_by_user",
        "banking_events_ready",
        "banking_jobs_due",
        "banking_observations_undelivered",
        "banking_resources_by_connection",
    ] {
        assert!(indexes.iter().any(|actual| actual == index), "missing {index}");
    }
}

#[tokio::test]
async fn schema_enforces_tenant_revision_and_encrypted_credential_constraints() {
    let (_verified, pool) = v2_test_support::fresh_v2_runtime().await;
    let user_id = Uuid::new_v4();
    let other_user = Uuid::new_v4();
    let connection_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO banking.provider_connections (
            id, user_id, provider, state, active_credential_ciphertext,
            active_credential_nonce, active_credential_key_id, active_credential_envelope_version,
            webhook_credential_ciphertext, webhook_credential_nonce,
            webhook_credential_key_id, webhook_credential_envelope_version, webhook_lookup_digest
         ) VALUES ($1,$2,'monobank','active',$3,$4,'key-1',1,$5,$6,'key-1',1,$7)",
    )
    .bind(connection_id)
    .bind(user_id)
    .bind(vec![1_u8, 2])
    .bind(vec![3_u8; 12])
    .bind(vec![4_u8, 5])
    .bind(vec![6_u8; 12])
    .bind(vec![7_u8; 32])
    .execute(&pool)
    .await
    .unwrap();

    let resource_id = Uuid::new_v4();
    let cross_tenant = sqlx::query(
        "INSERT INTO banking.external_resources (
            id,user_id,connection_id,external_resource_id,kind,funding_model,currency,masked_label
         ) VALUES ($1,$2,$3,'resource','card','own_funds','UAH','•••• 1234')",
    )
    .bind(resource_id)
    .bind(other_user)
    .bind(connection_id)
    .execute(&pool)
    .await;
    assert!(cross_tenant.is_err());

    sqlx::query(
        "INSERT INTO banking.external_resources (
            id,user_id,connection_id,external_resource_id,kind,funding_model,currency,masked_label
         ) VALUES ($1,$2,$3,'resource','card','own_funds','UAH','•••• 1234')",
    )
    .bind(resource_id)
    .bind(user_id)
    .bind(connection_id)
    .execute(&pool)
    .await
    .unwrap();

    let event_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO banking.provider_events (
            id,user_id,connection_id,external_resource_id,external_event_id,revision,
            transaction_state,operation_amount,operation_currency,description,
            content_digest,effective_at
         ) VALUES ($1,$2,$3,$4,'event',1,'pending',-10,'UAH','coffee',$5,clock_timestamp())",
    )
    .bind(event_id)
    .bind(user_id)
    .bind(connection_id)
    .bind(resource_id)
    .bind(vec![8_u8; 32])
    .execute(&pool)
    .await
    .unwrap();
    let duplicate_revision = sqlx::query(
        "INSERT INTO banking.provider_events (
            id,user_id,connection_id,external_resource_id,external_event_id,revision,
            transaction_state,operation_amount,operation_currency,description,
            content_digest,effective_at
         ) VALUES ($1,$2,$3,$4,'event',1,'settled',-10,'UAH','coffee',$5,clock_timestamp())",
    )
    .bind(Uuid::new_v4())
    .bind(user_id)
    .bind(connection_id)
    .bind(resource_id)
    .bind(vec![9_u8; 32])
    .execute(&pool)
    .await;
    assert!(duplicate_revision.is_err());
    assert!(sqlx::query("UPDATE banking.provider_events SET description='changed' WHERE id=$1")
        .bind(event_id)
        .execute(&pool)
        .await
        .is_err());

    let plaintext_columns: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM information_schema.columns WHERE table_schema='banking' \
         AND (column_name LIKE '%plain%' OR column_name IN ('token','webhook_secret','raw_payload'))",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(plaintext_columns, 0);
}

#[tokio::test]
async fn schema_uses_timestamptz_and_no_foreign_context_keys() {
    let (_verified, pool) = v2_test_support::fresh_v2_runtime().await;
    let wrong_timestamps: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM information_schema.columns \
         WHERE table_schema='banking' AND column_name LIKE '%_at' \
         AND data_type <> 'timestamp with time zone'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(wrong_timestamps, 0);

    let foreign_context_fks: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM information_schema.referential_constraints rc \
         JOIN information_schema.table_constraints tc \
           ON tc.constraint_name=rc.constraint_name AND tc.constraint_schema=rc.constraint_schema \
         JOIN information_schema.constraint_column_usage ccu \
           ON ccu.constraint_name=rc.unique_constraint_name AND ccu.constraint_schema=rc.unique_constraint_schema \
         WHERE tc.table_schema='banking' AND ccu.table_schema <> 'banking'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(foreign_context_fks, 0);

    let rows = sqlx::query("SELECT version, description FROM _sqlx_migrations ORDER BY version")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(rows.last().unwrap().get::<i64, _>("version"), 4);
    assert_eq!(rows.last().unwrap().get::<String, _>("description"), "banking");
}
