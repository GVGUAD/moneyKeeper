use std::sync::Arc;

use axum::http::StatusCode;
use axum_test::TestServer;
use serde_json::{Value, json};
use uuid::Uuid;

#[path = "v2_test_support.rs"]
mod v2_test_support;

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
    let database = v2_test_support::fresh_v2_database().await;
    let verified = database.initialize().await.unwrap();
    let server = TestServer::new(moneykeeper::bootstrap::v2::router(
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
