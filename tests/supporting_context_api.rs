use std::sync::Arc;

use axum::http::{Method, StatusCode, header::AUTHORIZATION};
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
    let database = test_support::fresh_database().await;
    let verified = database.initialize().await.unwrap();
    let router = moneykeeper::bootstrap::router(&verified, Arc::new(test_jwks()));
    let mut server = TestServer::new(router).unwrap();
    server.add_header(AUTHORIZATION, format!("Bearer {}", test_jwt(user_id)));
    server
}

#[tokio::test]
async fn every_supporting_route_requires_an_authenticated_user() {
    let database = test_support::fresh_database().await;
    let verified = database.initialize().await.unwrap();
    let server = TestServer::new(moneykeeper::bootstrap::router(
        &verified,
        Arc::new(test_jwks()),
    ))
    .unwrap();

    let id = Uuid::new_v4().to_string();
    for (method, route) in moneykeeper::api::routes::ROUTE_MANIFEST {
        if *route == "/oauth/gmail/callback" {
            continue;
        }
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

    let callback = server.get("/oauth/gmail/callback").await;
    assert_ne!(callback.status_code(), StatusCode::UNAUTHORIZED);

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
    let listed = body.as_array().unwrap();
    for code in [
        "UAH", "USD", "EUR", "AED", "AUD", "CAD", "CHF", "CNY", "CZK", "GBP", "GEL", "HUF", "ILS",
        "JPY", "PLN", "RON", "TRY",
    ] {
        assert!(
            listed.iter().any(|item| item["code"] == code),
            "public catalog omitted {code}"
        );
    }
    assert!(!listed.iter().any(|item| item["code"] == "RUB"));

    let get = server.get("/currencies/USD").await;
    assert_eq!(get.status_code(), StatusCode::OK);
    assert_eq!(get.json::<Value>()["minor_unit"], 2);

    let rub = server.get("/currencies/RUB").await;
    assert_eq!(rub.status_code(), StatusCode::NOT_FOUND);

    let invalid = server.get("/currencies/usd").await;
    assert_eq!(invalid.status_code(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn category_commands_require_versions_and_retain_archived_history() {
    let server = app(Uuid::new_v4()).await;
    let initial: Value = server.get("/categories").await.json();
    assert_eq!(initial["version"], 1);
    assert_eq!(initial["starter_template_version"], 1);
    assert_eq!(initial["roots"].as_array().unwrap().len(), 2);
    let created = server
        .post("/categories")
        .add_header("Idempotency-Key", "create-category")
        .json(&json!({"name": "Groceries", "kind": "expense", "expected_version": 1}))
        .await;
    assert_eq!(created.status_code(), StatusCode::CREATED);
    let category: Value = created.json();
    let id = category["node"]["id"].as_str().unwrap();
    assert_eq!(category["version"], 2);

    let fetched = server.get(&format!("/categories/{id}")).await;
    assert_eq!(fetched.status_code(), StatusCode::OK);
    assert_eq!(fetched.json::<Value>()["node"]["id"], id);

    let stale = server
        .patch(&format!("/categories/{id}"))
        .add_header("Idempotency-Key", "stale-category")
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
        .add_header("Idempotency-Key", "archive-category")
        .json(&json!({"expected_version": 2}))
        .await;
    assert_eq!(archived.status_code(), StatusCode::OK);
    let archived: Value = archived.json();
    assert_eq!(archived["node"]["local_lifecycle"], "archived");
    assert_eq!(archived["version"], 3);

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
    assert_eq!(listed["roots"].as_array().unwrap().len(), 3);
    assert_eq!(listed["roots"][2]["local_lifecycle"], "archived");

    let restored = server
        .post(&format!("/categories/{id}/restore"))
        .add_header("Idempotency-Key", "restore-category")
        .json(&json!({"expected_version": 3}))
        .await;
    assert_eq!(restored.status_code(), StatusCode::OK);
    assert_eq!(restored.json::<Value>()["version"], 4);
}

#[tokio::test]
async fn category_tree_styling_subtree_filters_and_retry_preserve_user_intent() {
    let user_id = Uuid::new_v4();
    let server = app(user_id).await;
    let tree: Value = server.get("/categories").await.json();
    let food = &tree["roots"][1]["children"][1];
    let group_id = food["id"].as_str().unwrap();
    let leaf_id = food["children"][0]["id"].as_str().unwrap();
    let icons: Vec<Value> = server.get("/category-icons").await.json();
    assert!(icons.iter().any(|icon| icon["key"] == "tag"));
    assert!(icons.iter().any(|icon| icon["key"] == "shopping-basket"));
    let changed = server
        .patch(&format!("/categories/{group_id}"))
        .add_header("Idempotency-Key", "style")
        .json(&json!({"expected_version":1,"color":"#abcdef","icon":null}))
        .await;
    changed.assert_status_ok();
    let changed: Value = changed.json();
    assert_eq!(changed["node"]["color"], "#ABCDEF");
    assert_eq!(changed["node"]["children"][0]["effective_color"], "#ABCDEF");
    assert_eq!(
        server
            .patch(&format!("/categories/{group_id}"))
            .add_header("Idempotency-Key", "style")
            .json(&json!({"expected_version":1,"color":"#abcdef","icon":null}))
            .await
            .json::<Value>(),
        changed
    );
    server
        .get(&format!("/categories/{leaf_id}"))
        .clear_headers()
        .authorization_bearer(test_jwt(Uuid::new_v4()))
        .await
        .assert_status_not_found();
    server
        .patch(&format!("/categories/{leaf_id}"))
        .clear_headers()
        .authorization_bearer(test_jwt(Uuid::new_v4()))
        .add_header("Idempotency-Key", "foreign-category")
        .json(&json!({"expected_version":999,"name":"Hidden"}))
        .await
        .assert_status_not_found();
    for patch in [
        json!({"expected_version":2,"color":"red"}),
        json!({"expected_version":2,"icon":"unknown"}),
        json!({"expected_version":2,"kind":"income"}),
    ] {
        server
            .patch(&format!("/categories/{leaf_id}"))
            .add_header("Idempotency-Key", Uuid::new_v4().to_string())
            .json(&patch)
            .await
            .assert_status_bad_request();
    }
    let account: Value = server.post("/accounts").add_header("Idempotency-Key", "wallet")
        .json(&json!({"name":"Wallet","currency":"UAH","kind":"cash","nature":"asset","opening_balance":"0.00"})).await.json();
    let posted = server.post("/transactions").add_header("Idempotency-Key", "purchase")
        .json(&json!({"account_id":account["account"]["id"],"kind":"expense","amount":{"amount":"50.00","currency":"UAH"},"description":"Groceries","category_id":leaf_id})).await;
    assert_eq!(
        posted.status_code(),
        StatusCode::CREATED,
        "{}",
        posted.text()
    );
    let id = posted.json::<Value>()["journal_entry_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let assigned: Value = server.get(&format!("/transactions/{id}")).await.json();
    assert_eq!(assigned["classification"]["assignment_origin"], "manual");
    assert_eq!(assigned["classification"]["automation_state"], "suppressed");
    let filtered = server
        .get(&format!("/transactions?category_id={group_id}"))
        .await;
    filtered.assert_status_ok();
    assert_eq!(filtered.json::<Vec<Value>>().len(), 1);
    server
        .get(&format!(
            "/transactions?category_id={group_id}&uncategorized=true"
        ))
        .await
        .assert_status_bad_request();
    server
        .post(&format!("/categories/{group_id}/archive"))
        .add_header("Idempotency-Key", "archive-group")
        .json(&json!({"expected_version":2}))
        .await
        .assert_status_ok();
    assert_eq!(
        server
            .get(&format!("/transactions?category_id={group_id}"))
            .await
            .json::<Vec<Value>>()
            .len(),
        1
    );
    let cleared = server
        .patch(&format!("/transactions/{id}/annotation"))
        .add_header("Idempotency-Key", "clear")
        .json(&json!({"expected_version":1,"clear_category":true}))
        .await;
    cleared.assert_status_ok();
    let replay = server
        .patch(&format!("/transactions/{id}/annotation"))
        .add_header("Idempotency-Key", "clear")
        .json(&json!({"expected_version":1,"clear_category":true}))
        .await;
    replay.assert_status_ok();
    assert_eq!(replay.json::<Value>()["replayed"], true);
    let retry = server
        .post(&format!("/transactions/{id}/classification/retry"))
        .add_header("Idempotency-Key", "retry")
        .json(&json!({"expected_annotation_version":2}))
        .await;
    assert_eq!(
        retry.status_code(),
        StatusCode::ACCEPTED,
        "{}",
        retry.text()
    );
    let retry = server
        .post(&format!("/transactions/{id}/classification/retry"))
        .add_header("Idempotency-Key", "retry")
        .json(&json!({"expected_annotation_version":2}))
        .await;
    assert_eq!(retry.status_code(), StatusCode::ACCEPTED);
    assert_eq!(retry.json::<Value>()["replayed"], true);
    assert_eq!(
        server
            .get("/transactions?uncategorized=true")
            .await
            .json::<Vec<Value>>()
            .len(),
        1
    );
    let restored = server
        .post(&format!("/categories/{group_id}/restore"))
        .add_header("Idempotency-Key", "restore-group")
        .json(&json!({"expected_version":3}))
        .await;
    restored.assert_status_ok();
    let moved = server
        .post(&format!("/categories/{leaf_id}/move"))
        .add_header("Idempotency-Key", "move-leaf")
        .json(&json!({"expected_version":4,"parent_id":group_id,"target_position":1}))
        .await;
    moved.assert_status_ok();
    let siblings: Value = server.get(&format!("/categories/{group_id}")).await.json();
    assert_eq!(siblings["node"]["children"][1]["id"], leaf_id);
    let sibling_ids = siblings["node"]["children"]
        .as_array()
        .unwrap()
        .iter()
        .rev()
        .map(|node| node["id"].clone())
        .collect::<Vec<_>>();
    server
        .put("/categories/reorder")
        .add_header("Idempotency-Key", "reorder-leaves")
        .json(
            &json!({"expected_version":5,"parent_id":group_id,"ordered_category_ids":sibling_ids}),
        )
        .await
        .assert_status_ok();
    let reordered: Value = server.get(&format!("/categories/{group_id}")).await.json();
    assert_eq!(reordered["version"], 6);
    assert_eq!(reordered["node"]["children"][0]["id"], leaf_id);
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
        .json(&json!({"base_currency": "RUB", "expected_version": 1}))
        .await;
    assert_eq!(invalid.status_code(), StatusCode::BAD_REQUEST);
}
