use axum::http::StatusCode;
use serde_json::Value;
use uuid::Uuid;

use super::{common, helpers};

#[tokio::test]
async fn create_account_cash_returns_201() {
    let postgres = common::TestPostgres::new().await;
    let server = helpers::make_app(postgres.pool).await;
    let (_uid, token) = helpers::create_test_user();

    let res = server
        .post("/accounts")
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .json(&serde_json::json!({
            "name": "Wallet",
            "account_type": "Cash",
            "currency": "USD"
        }))
        .await;

    assert_eq!(res.status_code(), StatusCode::CREATED);
    let body: Value = res.json();
    assert!(body["id"].is_string());
    assert_eq!(body["account_type"], "Cash");
}

#[tokio::test]
async fn create_account_savings_returns_201() {
    let postgres = common::TestPostgres::new().await;
    let server = helpers::make_app(postgres.pool).await;
    let (_uid, token) = helpers::create_test_user();

    let res = server
        .post("/accounts")
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .json(&serde_json::json!({
            "name": "Savings",
            "account_type": "Savings",
            "currency": "USD",
            "details": {
                "type": "savings",
                "interest_rate": "3.5",
                "compounding_period": "Monthly"
            }
        }))
        .await;

    assert_eq!(res.status_code(), StatusCode::CREATED);
    let body: Value = res.json();
    assert_eq!(body["details"]["type"], "savings");
    assert_eq!(body["details"]["compounding_period"], "Monthly");
}

#[tokio::test]
async fn create_account_invalid_type_returns_400() {
    let postgres = common::TestPostgres::new().await;
    let server = helpers::make_app(postgres.pool).await;
    let (_uid, token) = helpers::create_test_user();

    let res = server
        .post("/accounts")
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .json(&serde_json::json!({
            "name": "Test",
            "account_type": "InvalidType",
            "currency": "USD"
        }))
        .await;

    assert_eq!(res.status_code(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn create_account_no_auth_returns_401() {
    let postgres = common::TestPostgres::new().await;
    let server = helpers::make_app(postgres.pool).await;

    let res = server
        .post("/accounts")
        .json(&serde_json::json!({
            "name": "Test",
            "account_type": "Cash",
            "currency": "USD"
        }))
        .await;

    assert_eq!(res.status_code(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn list_accounts_empty_returns_empty_array() {
    let postgres = common::TestPostgres::new().await;
    let server = helpers::make_app(postgres.pool).await;
    let (_uid, token) = helpers::create_test_user();

    let res = server
        .get("/accounts")
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .await;

    assert_eq!(res.status_code(), StatusCode::OK);
    let body: Value = res.json();
    assert_eq!(body, serde_json::json!([]));
}

#[tokio::test]
async fn list_accounts_returns_only_own() {
    let postgres = common::TestPostgres::new().await;
    let server = helpers::make_app(postgres.pool).await;
    let (_uid1, token1) = helpers::create_test_user();
    let (_uid2, token2) = helpers::create_test_user();

    helpers::create_account_for(&server, &token1).await;
    helpers::create_account_for(&server, &token1).await;
    helpers::create_account_for(&server, &token2).await;

    let res1 = server
        .get("/accounts")
        .add_header(helpers::auth(&token1).0, helpers::auth(&token1).1)
        .await;
    let body1: Value = res1.json();
    assert_eq!(body1.as_array().unwrap().len(), 2);

    let res2 = server
        .get("/accounts")
        .add_header(helpers::auth(&token2).0, helpers::auth(&token2).1)
        .await;
    let body2: Value = res2.json();
    assert_eq!(body2.as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn get_account_returns_correct_data() {
    let postgres = common::TestPostgres::new().await;
    let server = helpers::make_app(postgres.pool).await;
    let (_uid, token) = helpers::create_test_user();
    let account_id = helpers::create_account_for(&server, &token).await;

    let res = server
        .get(&format!("/accounts/{account_id}"))
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .await;

    assert_eq!(res.status_code(), StatusCode::OK);
    let body: Value = res.json();
    assert_eq!(body["id"].as_str().unwrap(), account_id.to_string());
    assert_eq!(body["name"], "Test Account");
    assert_eq!(body["currency"], "USD");
}

#[tokio::test]
async fn get_account_not_found_returns_404() {
    let postgres = common::TestPostgres::new().await;
    let server = helpers::make_app(postgres.pool).await;
    let (_uid, token) = helpers::create_test_user();
    let random_id = Uuid::new_v4();

    let res = server
        .get(&format!("/accounts/{random_id}"))
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .await;

    assert_eq!(res.status_code(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn get_account_other_users_returns_404() {
    let postgres = common::TestPostgres::new().await;
    let server = helpers::make_app(postgres.pool).await;
    let (_uid1, token1) = helpers::create_test_user();
    let (_uid2, token2) = helpers::create_test_user();

    let account_id = helpers::create_account_for(&server, &token1).await;

    let res = server
        .get(&format!("/accounts/{account_id}"))
        .add_header(helpers::auth(&token2).0, helpers::auth(&token2).1)
        .await;

    assert_eq!(res.status_code(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn update_account_name_returns_200() {
    let postgres = common::TestPostgres::new().await;
    let server = helpers::make_app(postgres.pool).await;
    let (_uid, token) = helpers::create_test_user();
    let account_id = helpers::create_account_for(&server, &token).await;

    let res = server
        .put(&format!("/accounts/{account_id}"))
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .json(&serde_json::json!({ "name": "Updated Name" }))
        .await;

    assert_eq!(res.status_code(), StatusCode::OK);
    let body: Value = res.json();
    assert_eq!(body["name"], "Updated Name");
}

#[tokio::test]
async fn update_account_not_found_returns_404() {
    let postgres = common::TestPostgres::new().await;
    let server = helpers::make_app(postgres.pool).await;
    let (_uid, token) = helpers::create_test_user();
    let random_id = Uuid::new_v4();

    let res = server
        .put(&format!("/accounts/{random_id}"))
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .json(&serde_json::json!({ "name": "New Name" }))
        .await;

    assert_eq!(res.status_code(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn delete_account_returns_204() {
    let postgres = common::TestPostgres::new().await;
    let server = helpers::make_app(postgres.pool).await;
    let (_uid, token) = helpers::create_test_user();
    let account_id = helpers::create_account_for(&server, &token).await;

    let res = server
        .delete(&format!("/accounts/{account_id}"))
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .await;

    assert_eq!(res.status_code(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn delete_account_not_found_returns_404() {
    let postgres = common::TestPostgres::new().await;
    let server = helpers::make_app(postgres.pool).await;
    let (_uid, token) = helpers::create_test_user();
    let random_id = Uuid::new_v4();

    let res = server
        .delete(&format!("/accounts/{random_id}"))
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .await;

    assert_eq!(res.status_code(), StatusCode::NOT_FOUND);
}