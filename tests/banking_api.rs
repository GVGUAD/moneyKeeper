use std::sync::Arc;

use axum::http::StatusCode;
use axum_test::TestServer;
use serde_json::{Value, json};
use uuid::Uuid;

#[path = "test_support.rs"]
mod test_support;

const TEST_KID: &str = "test-key-1";
const TEST_EC_PRIVATE_KEY: &str = "-----BEGIN PRIVATE KEY-----
MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgN/zCeuQq48O/tp5y
b50qbJqKns6bCt6JctzISKw2sPqhRANCAASdCO4KOpDloBoTURgT/ZeiWey7OSIG
46TJaP8IugkOaxHZ6HCuZvK4AaDOXLZHOyRHEWK5AhPl1f98M4xYBkQy
-----END PRIVATE KEY-----";

fn test_jwks() -> jsonwebtoken::jwk::JwkSet {
    serde_json::from_value(json!({"keys": [{
        "kty": "EC", "crv": "P-256", "kid": TEST_KID, "alg": "ES256", "use": "sig",
        "x": "nQjuCjqQ5aAaE1EYE_2XolnsuzkiBuOkyWj_CLoJDms",
        "y": "EdnocK5m8rgBoM5ctkc7JEcRYrkCE-XV_3wzjFgGRDI"
    }]}))
    .unwrap()
}

fn jwt(user_id: Uuid) -> String {
    use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
    #[derive(serde::Serialize)]
    struct Claims {
        sub: String,
        aud: String,
        role: String,
        exp: i64,
        iat: i64,
    }
    let now = chrono::Utc::now().timestamp();
    let mut header = Header::new(Algorithm::ES256);
    header.kid = Some(TEST_KID.to_owned());
    encode(
        &header,
        &Claims {
            sub: user_id.to_string(),
            aud: "authenticated".to_owned(),
            role: "authenticated".to_owned(),
            exp: now + 3_600,
            iat: now,
        },
        &EncodingKey::from_ec_pem(TEST_EC_PRIVATE_KEY.as_bytes()).unwrap(),
    )
    .unwrap()
}

#[tokio::test]
async fn banking_surface_is_tenant_scoped_idempotent_and_never_echoes_tokens() {
    let database = test_support::fresh_database().await;
    let verified = database.initialize().await.unwrap();
    let server = TestServer::new(moneykeeper::bootstrap::router(
        &verified,
        Arc::new(test_jwks()),
    ))
    .unwrap();
    let owner = jwt(Uuid::new_v4());
    let stranger = jwt(Uuid::new_v4());

    let missing_auth = server.get("/provider-connections").await;
    assert_eq!(missing_auth.status_code(), StatusCode::UNAUTHORIZED);

    let first = server
        .post("/provider-connections/monobank")
        .authorization_bearer(&owner)
        .add_header("Idempotency-Key", "connect-api")
        .json(&json!({"x_token": "super-secret-provider-token"}))
        .await;
    assert_eq!(first.status_code(), StatusCode::ACCEPTED);
    let first_body: Value = first.json();
    assert_eq!(first_body["replayed"], false);
    assert!(
        !first_body
            .to_string()
            .contains("super-secret-provider-token")
    );
    assert!(first_body.get("credential").is_none());
    let id = first_body["connection"]["id"].as_str().unwrap();

    let replay = server
        .post("/provider-connections/monobank")
        .authorization_bearer(&owner)
        .add_header("Idempotency-Key", "connect-api")
        .json(&json!({"x_token": "super-secret-provider-token"}))
        .await;
    assert_eq!(replay.status_code(), StatusCode::ACCEPTED);
    let replay_body: Value = replay.json();
    assert_eq!(replay_body["replayed"], true);
    assert_eq!(replay_body["connection"]["id"], id);

    let conflict = server
        .post("/provider-connections/monobank")
        .authorization_bearer(&owner)
        .add_header("Idempotency-Key", "connect-api")
        .json(&json!({"x_token": "different-provider-token"}))
        .await;
    assert_eq!(conflict.status_code(), StatusCode::CONFLICT);

    let replacement = server
        .post(&format!(
            "/provider-connections/{id}/credential-replacements"
        ))
        .authorization_bearer(&owner)
        .add_header("Idempotency-Key", "replace-api")
        .json(&json!({
            "x_token": "replacement-provider-token",
            "expected_version": 1
        }))
        .await;
    assert_eq!(replacement.status_code(), StatusCode::ACCEPTED);
    let replacement_body: Value = replacement.json();
    assert_eq!(replacement_body["connection"]["id"], id);
    assert_eq!(replacement_body["replayed"], false);
    assert!(
        !replacement_body
            .to_string()
            .contains("replacement-provider-token")
    );
    let replacement_replay = server
        .post(&format!(
            "/provider-connections/{id}/credential-replacements"
        ))
        .authorization_bearer(&owner)
        .add_header("Idempotency-Key", "replace-api")
        .json(&json!({
            "x_token": "replacement-provider-token",
            "expected_version": 1
        }))
        .await;
    assert_eq!(replacement_replay.status_code(), StatusCode::ACCEPTED);
    assert_eq!(replacement_replay.json::<Value>()["replayed"], true);
    let replacement_conflict = server
        .post(&format!(
            "/provider-connections/{id}/credential-replacements"
        ))
        .authorization_bearer(&owner)
        .add_header("Idempotency-Key", "replace-api")
        .json(&json!({
            "x_token": "conflicting-replacement-token",
            "expected_version": 1
        }))
        .await;
    assert_eq!(replacement_conflict.status_code(), StatusCode::CONFLICT);

    let owner_get = server
        .get(&format!("/provider-connections/{id}"))
        .authorization_bearer(&owner)
        .await;
    assert_eq!(owner_get.status_code(), StatusCode::OK);
    let stranger_get = server
        .get(&format!("/provider-connections/{id}"))
        .authorization_bearer(&stranger)
        .await;
    assert_eq!(stranger_get.status_code(), StatusCode::NOT_FOUND);

    let callback = server.get("/webhooks/monobank/not-a-credential").await;
    assert_eq!(callback.status_code(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn resource_paths_are_connection_scoped_and_lists_include_current_mapping() {
    let (verified, pool) = test_support::fresh_runtime().await;
    let user_id = Uuid::new_v4();
    let connection_a = Uuid::new_v4();
    let connection_b = Uuid::new_v4();
    for connection_id in [connection_a, connection_b] {
        sqlx::query(
            "INSERT INTO banking.provider_connections (id,user_id,provider,state) \
             VALUES ($1,$2,'monobank','pending')",
        )
        .bind(connection_id)
        .bind(user_id)
        .execute(&pool)
        .await
        .unwrap();
    }
    let resource_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO banking.external_resources \
         (id,user_id,connection_id,external_resource_id,kind,funding_model,currency,masked_label) \
         VALUES ($1,$2,$3,'resource-b','card','own_funds','UAH','•••• 1234')",
    )
    .bind(resource_id)
    .bind(user_id)
    .bind(connection_b)
    .execute(&pool)
    .await
    .unwrap();
    let mapping_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO banking.resource_mappings \
         (id,user_id,connection_id,external_resource_id,mapping_version,state,process_correlation_id,effective_at) \
         VALUES ($1,$2,$3,$4,1,'pending_account_creation',$5,clock_timestamp())",
    )
    .bind(mapping_id)
    .bind(user_id)
    .bind(connection_b)
    .bind(resource_id)
    .bind(Uuid::new_v4())
    .execute(&pool)
    .await
    .unwrap();

    let server = TestServer::new(moneykeeper::bootstrap::router(
        &verified,
        Arc::new(test_jwks()),
    ))
    .unwrap();
    let token = jwt(user_id);
    let resources: Value = server
        .get(&format!("/provider-connections/{connection_b}/resources"))
        .authorization_bearer(&token)
        .await
        .json();
    assert_eq!(
        resources[0]["current_mapping"]["id"],
        mapping_id.to_string()
    );
    assert_eq!(
        resources[0]["current_mapping"]["state"],
        "pending_account_creation"
    );

    let mismatched_resource = server
        .post(&format!(
            "/provider-connections/{connection_a}/resource-mappings"
        ))
        .authorization_bearer(&token)
        .add_header("Idempotency-Key", "mismatched-resource")
        .json(&json!({
            "resource_id": resource_id,
            "account_name": "Mapped card",
            "expected_version": 1
        }))
        .await;
    assert_eq!(mismatched_resource.status_code(), StatusCode::NOT_FOUND);

    let mismatched_mapping = server
        .post(&format!(
            "/provider-connections/{connection_a}/resource-mappings/{mapping_id}/deactivations"
        ))
        .authorization_bearer(&token)
        .add_header("Idempotency-Key", "mismatched-mapping")
        .json(&json!({"expected_version":1,"reason":"Disconnect"}))
        .await;
    assert_eq!(mismatched_mapping.status_code(), StatusCode::NOT_FOUND);
    let state: String = sqlx::query_scalar(
        "SELECT state FROM banking.resource_mappings WHERE id=$1 AND user_id=$2",
    )
    .bind(mapping_id)
    .bind(user_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(state, "pending_account_creation");
}
