use std::sync::Arc;

use axum::http::{Method, StatusCode, header::AUTHORIZATION};
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
    serde_json::from_value(json!({
        "keys": [{
            "kty": "EC",
            "crv": "P-256",
            "kid": TEST_KID,
            "alg": "ES256",
            "use": "sig",
            "x": "nQjuCjqQ5aAaE1EYE_2XolnsuzkiBuOkyWj_CLoJDms",
            "y": "EdnocK5m8rgBoM5ctkc7JEcRYrkCE-XV_3wzjFgGRDI"
        }]
    }))
    .unwrap()
}

fn test_jwt(user_id: Uuid) -> String {
    use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};

    #[derive(serde::Serialize)]
    struct TestClaims {
        sub: String,
        aud: String,
        role: String,
        exp: i64,
        iat: i64,
    }

    let now = chrono::Utc::now().timestamp();
    let claims = TestClaims {
        sub: user_id.to_string(),
        aud: "authenticated".to_owned(),
        role: "authenticated".to_owned(),
        exp: now + 3_600,
        iat: now,
    };
    let mut header = Header::new(Algorithm::ES256);
    header.kid = Some(TEST_KID.to_owned());
    encode(
        &header,
        &claims,
        &EncodingKey::from_ec_pem(TEST_EC_PRIVATE_KEY.as_bytes()).unwrap(),
    )
    .unwrap()
}

async fn app(user_id: Uuid) -> TestServer {
    let database = v2_test_support::fresh_v2_database().await;
    let verified = database.initialize().await.unwrap();
    let router = moneykeeper::bootstrap::v2::router(&verified, Arc::new(test_jwks()));
    let mut server = TestServer::new(router).unwrap();
    server.add_header(AUTHORIZATION, format!("Bearer {}", test_jwt(user_id)));
    server
}

#[tokio::test]
async fn every_supporting_route_requires_an_authenticated_user() {
    let database = v2_test_support::fresh_v2_database().await;
    let verified = database.initialize().await.unwrap();
    let server = TestServer::new(moneykeeper::bootstrap::v2::router(
        &verified,
        Arc::new(test_jwks()),
    ))
    .unwrap();

    let id = Uuid::new_v4().to_string();
    for (method, route) in moneykeeper::api::v2::ROUTE_MANIFEST {
        let path = route
            .replace("{code}", "USD")
            .replace("{id}", &id)
            .replace("{mapping_id}", &id);
        let response = server
            .method(Method::from_bytes(method.as_bytes()).unwrap(), &path)
            .await;
        assert_eq!(
            response.status_code(),
            StatusCode::UNAUTHORIZED,
            "{method} {path} accepted an unauthenticated request"
        );
    }

    let invalid = server
        .get("/currencies")
        .authorization_bearer("not-a-valid-jwt")
        .await;
    assert_eq!(invalid.status_code(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn currency_routes_are_exact_and_side_effect_free() {
    let server = app(Uuid::new_v4()).await;
    let list = server.get("/currencies").await;
    assert_eq!(list.status_code(), StatusCode::OK);
    let body: Value = list.json();
    assert!(
        body.as_array()
            .unwrap()
            .iter()
            .any(|item| item["code"] == "UAH")
    );

    let get = server.get("/currencies/USD").await;
    assert_eq!(get.status_code(), StatusCode::OK);
    assert_eq!(get.json::<Value>()["minor_unit"], 2);

    let invalid = server.get("/currencies/usd").await;
    assert_eq!(invalid.status_code(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn category_commands_require_versions_and_retain_archived_history() {
    let server = app(Uuid::new_v4()).await;
    let created = server
        .post("/categories")
        .json(&json!({"name": "Groceries", "kind": "expense"}))
        .await;
    assert_eq!(created.status_code(), StatusCode::CREATED);
    let category: Value = created.json();
    let id = category["id"].as_str().unwrap();
    assert_eq!(category["version"], 1);

    let fetched = server.get(&format!("/categories/{id}")).await;
    assert_eq!(fetched.status_code(), StatusCode::OK);
    assert_eq!(fetched.json::<Value>()["id"], id);

    let stale = server
        .patch(&format!("/categories/{id}"))
        .json(&json!({"name": "Food", "expected_version": 9}))
        .await;
    assert_eq!(stale.status_code(), StatusCode::CONFLICT);

    let missing_version = server
        .patch(&format!("/categories/{id}"))
        .json(&json!({"name": "Food"}))
        .await;
    assert_eq!(missing_version.status_code(), StatusCode::BAD_REQUEST);
    assert_eq!(
        missing_version.json::<Value>()["error"],
        "invalid JSON request"
    );

    let malformed = server
        .patch(&format!("/categories/{id}"))
        .text("{")
        .content_type("application/json")
        .await;
    assert_eq!(malformed.status_code(), StatusCode::BAD_REQUEST);
    assert_eq!(malformed.json::<Value>()["error"], "invalid JSON request");

    let invalid_rename_version = server
        .patch(&format!("/categories/{id}"))
        .json(&json!({"name": "Food", "expected_version": 0}))
        .await;
    assert_eq!(
        invalid_rename_version.status_code(),
        StatusCode::BAD_REQUEST
    );

    let invalid_archive_version = server
        .post(&format!("/categories/{id}/archive"))
        .json(&json!({"expected_version": -1}))
        .await;
    assert_eq!(
        invalid_archive_version.status_code(),
        StatusCode::BAD_REQUEST
    );

    let archived = server
        .post(&format!("/categories/{id}/archive"))
        .json(&json!({"expected_version": 1}))
        .await;
    assert_eq!(archived.status_code(), StatusCode::OK);
    let archived: Value = archived.json();
    assert_eq!(archived["lifecycle"], "archived");
    assert_eq!(archived["version"], 2);

    let invalid_restore_version = server
        .post(&format!("/categories/{id}/restore"))
        .json(&json!({"expected_version": 0}))
        .await;
    assert_eq!(
        invalid_restore_version.status_code(),
        StatusCode::BAD_REQUEST
    );

    let listed = server.get("/categories").await;
    assert_eq!(listed.status_code(), StatusCode::OK);
    let listed: Value = listed.json();
    assert_eq!(listed.as_array().unwrap().len(), 1);
    assert_eq!(listed[0]["lifecycle"], "archived");

    let restored = server
        .post(&format!("/categories/{id}/restore"))
        .json(&json!({"expected_version": 2}))
        .await;
    assert_eq!(restored.status_code(), StatusCode::OK);
    assert_eq!(restored.json::<Value>()["version"], 3);
}

#[tokio::test]
async fn preferences_read_is_non_persisting_and_update_is_compare_and_swap() {
    let server = app(Uuid::new_v4()).await;
    let initial = server.get("/preferences").await;
    assert_eq!(initial.status_code(), StatusCode::OK);
    let initial: Value = initial.json();
    assert_eq!(initial["base_currency"], "UAH");
    assert_eq!(initial["version"], 0);
    assert_eq!(initial["persisted"], false);

    let updated = server
        .patch("/preferences")
        .json(&json!({"base_currency": "USD", "expected_version": 0}))
        .await;
    assert_eq!(updated.status_code(), StatusCode::OK);
    let updated: Value = updated.json();
    assert_eq!(updated["base_currency"], "USD");
    assert_eq!(updated["version"], 1);
    assert_eq!(updated["persisted"], true);

    let stale = server
        .patch("/preferences")
        .json(&json!({"base_currency": "EUR", "expected_version": 0}))
        .await;
    assert_eq!(stale.status_code(), StatusCode::CONFLICT);

    let invalid_version = server
        .patch("/preferences")
        .json(&json!({"base_currency": "EUR", "expected_version": -1}))
        .await;
    assert_eq!(invalid_version.status_code(), StatusCode::BAD_REQUEST);

    let invalid = server
        .patch("/preferences")
        .json(&json!({"base_currency": "GBP", "expected_version": 1}))
        .await;
    assert_eq!(invalid.status_code(), StatusCode::BAD_REQUEST);
}
