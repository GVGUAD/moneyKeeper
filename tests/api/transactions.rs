use axum::http::StatusCode;
use serde_json::Value;
use uuid::Uuid;

use super::{common, helpers};

async fn create_tx(
    server: &axum_test::TestServer,
    token: &str,
    account_id: Uuid,
    kind: &str,
    amount: &str,
) -> Uuid {
    let res = server
        .post(&format!("/accounts/{account_id}/transactions"))
        .add_header(helpers::auth(token).0, helpers::auth(token).1)
        .json(&serde_json::json!({
            "amount": amount,
            "currency": "USD",
            "kind": kind,
            "transacted_at": "2024-01-01T00:00:00Z"
        }))
        .await;
    Uuid::parse_str(res.json::<Value>()["id"].as_str().unwrap()).unwrap()
}

#[tokio::test]
async fn create_transaction_income_returns_201() {
    let postgres = common::TestPostgres::new().await;
    let server = helpers::make_app(postgres.pool).await;
    let (_uid, token) = helpers::create_test_user();
    let account_id = helpers::create_account_for(&server, &token).await;

    let res = server
        .post(&format!("/accounts/{account_id}/transactions"))
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .json(&serde_json::json!({
            "amount": "100.00",
            "currency": "USD",
            "kind": "Income",
            "transacted_at": "2024-01-01T00:00:00Z"
        }))
        .await;

    assert_eq!(res.status_code(), StatusCode::CREATED);
    let body: Value = res.json();
    assert!(body["id"].is_string());
    assert_eq!(body["kind"], "Income");
}

#[tokio::test]
async fn create_transaction_expense_returns_201() {
    let postgres = common::TestPostgres::new().await;
    let server = helpers::make_app(postgres.pool).await;
    let (_uid, token) = helpers::create_test_user();
    let account_id = helpers::create_account_for(&server, &token).await;

    let res = server
        .post(&format!("/accounts/{account_id}/transactions"))
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .json(&serde_json::json!({
            "amount": "50.00",
            "currency": "USD",
            "kind": "Expense",
            "transacted_at": "2024-01-01T00:00:00Z"
        }))
        .await;

    assert_eq!(res.status_code(), StatusCode::CREATED);
    let body: Value = res.json();
    assert_eq!(body["kind"], "Expense");
}

#[tokio::test]
async fn create_transaction_with_category_returns_201() {
    let postgres = common::TestPostgres::new().await;
    let server = helpers::make_app(postgres.pool).await;
    let (_uid, token) = helpers::create_test_user();
    let account_id = helpers::create_account_for(&server, &token).await;
    let cat_id = helpers::create_category_for(&server, &token).await;

    let res = server
        .post(&format!("/accounts/{account_id}/transactions"))
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .json(&serde_json::json!({
            "amount": "25",
            "currency": "USD",
            "kind": "Expense",
            "category_id": cat_id,
            "transacted_at": "2024-01-01T00:00:00Z"
        }))
        .await;

    assert_eq!(res.status_code(), StatusCode::CREATED);
    let body: Value = res.json();
    assert_eq!(body["category_id"].as_str().unwrap(), cat_id.to_string());
}

#[tokio::test]
async fn create_transaction_invalid_kind_returns_400() {
    let postgres = common::TestPostgres::new().await;
    let server = helpers::make_app(postgres.pool).await;
    let (_uid, token) = helpers::create_test_user();
    let account_id = helpers::create_account_for(&server, &token).await;

    let res = server
        .post(&format!("/accounts/{account_id}/transactions"))
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .json(&serde_json::json!({
            "amount": "10",
            "currency": "USD",
            "kind": "InvalidKind",
            "transacted_at": "2024-01-01T00:00:00Z"
        }))
        .await;

    assert_eq!(res.status_code(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn create_transaction_account_not_found_returns_404() {
    let postgres = common::TestPostgres::new().await;
    let server = helpers::make_app(postgres.pool).await;
    let (_uid, token) = helpers::create_test_user();
    let random_id = Uuid::new_v4();

    let res = server
        .post(&format!("/accounts/{random_id}/transactions"))
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .json(&serde_json::json!({
            "amount": "10",
            "currency": "USD",
            "kind": "Income",
            "transacted_at": "2024-01-01T00:00:00Z"
        }))
        .await;

    assert_eq!(res.status_code(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn create_transaction_no_auth_returns_401() {
    let postgres = common::TestPostgres::new().await;
    let server = helpers::make_app(postgres.pool).await;
    let (_uid, token) = helpers::create_test_user();
    let account_id = helpers::create_account_for(&server, &token).await;

    let res = server
        .post(&format!("/accounts/{account_id}/transactions"))
        .json(&serde_json::json!({
            "amount": "10",
            "currency": "USD",
            "kind": "Income",
            "transacted_at": "2024-01-01T00:00:00Z"
        }))
        .await;

    assert_eq!(res.status_code(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn list_account_transactions_returns_own() {
    let postgres = common::TestPostgres::new().await;
    let server = helpers::make_app(postgres.pool).await;
    let (_uid, token) = helpers::create_test_user();
    let account_id = helpers::create_account_for(&server, &token).await;

    create_tx(&server, &token, account_id, "Income", "100").await;
    create_tx(&server, &token, account_id, "Expense", "50").await;

    let res = server
        .get(&format!("/accounts/{account_id}/transactions"))
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .await;

    assert_eq!(res.status_code(), StatusCode::OK);
    let body: Value = res.json();
    assert_eq!(body.as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn list_account_transactions_filter_by_kind() {
    let postgres = common::TestPostgres::new().await;
    let server = helpers::make_app(postgres.pool).await;
    let (_uid, token) = helpers::create_test_user();
    let account_id = helpers::create_account_for(&server, &token).await;

    create_tx(&server, &token, account_id, "Income", "100").await;
    create_tx(&server, &token, account_id, "Income", "200").await;
    create_tx(&server, &token, account_id, "Expense", "50").await;

    let res = server
        .get(&format!("/accounts/{account_id}/transactions?kind=Income"))
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .await;

    let body: Value = res.json();
    let items = body.as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert!(items.iter().all(|t| t["kind"] == "Income"));
}

#[tokio::test]
async fn list_account_transactions_pagination() {
    let postgres = common::TestPostgres::new().await;
    let server = helpers::make_app(postgres.pool).await;
    let (_uid, token) = helpers::create_test_user();
    let account_id = helpers::create_account_for(&server, &token).await;

    create_tx(&server, &token, account_id, "Income", "1").await;
    create_tx(&server, &token, account_id, "Income", "2").await;
    create_tx(&server, &token, account_id, "Income", "3").await;

    let res_page1 = server
        .get(&format!(
            "/accounts/{account_id}/transactions?limit=1&offset=0"
        ))
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .await;
    assert_eq!(res_page1.json::<Value>().as_array().unwrap().len(), 1);

    let res_page2 = server
        .get(&format!(
            "/accounts/{account_id}/transactions?limit=1&offset=1"
        ))
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .await;
    assert_eq!(res_page2.json::<Value>().as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn list_all_transactions_spans_accounts() {
    let postgres = common::TestPostgres::new().await;
    let server = helpers::make_app(postgres.pool).await;
    let (_uid, token) = helpers::create_test_user();
    let acc1 = helpers::create_account_for(&server, &token).await;
    let acc2 = helpers::create_account_for(&server, &token).await;

    create_tx(&server, &token, acc1, "Income", "10").await;
    create_tx(&server, &token, acc2, "Expense", "5").await;

    let res = server
        .get("/transactions")
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .await;

    assert_eq!(res.status_code(), StatusCode::OK);
    let body: Value = res.json();
    assert_eq!(body.as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn list_all_transactions_no_auth_returns_401() {
    let postgres = common::TestPostgres::new().await;
    let server = helpers::make_app(postgres.pool).await;

    let res = server.get("/transactions").await;

    assert_eq!(res.status_code(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn get_transaction_returns_correct_data() {
    let postgres = common::TestPostgres::new().await;
    let server = helpers::make_app(postgres.pool).await;
    let (_uid, token) = helpers::create_test_user();
    let account_id = helpers::create_account_for(&server, &token).await;
    let tx_id = create_tx(&server, &token, account_id, "Income", "42").await;

    let res = server
        .get(&format!("/transactions/{tx_id}"))
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .await;

    assert_eq!(res.status_code(), StatusCode::OK);
    let body: Value = res.json();
    assert_eq!(body["id"].as_str().unwrap(), tx_id.to_string());
    assert_eq!(body["kind"], "Income");
    assert_eq!(body["amount"], "42");
}

#[tokio::test]
async fn get_transaction_not_found_returns_404() {
    let postgres = common::TestPostgres::new().await;
    let server = helpers::make_app(postgres.pool).await;
    let (_uid, token) = helpers::create_test_user();
    let random_id = Uuid::new_v4();

    let res = server
        .get(&format!("/transactions/{random_id}"))
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .await;

    assert_eq!(res.status_code(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn get_transaction_other_users_returns_404() {
    let postgres = common::TestPostgres::new().await;
    let server = helpers::make_app(postgres.pool).await;
    let (_uid1, token1) = helpers::create_test_user();
    let (_uid2, token2) = helpers::create_test_user();

    let acc = helpers::create_account_for(&server, &token1).await;
    let tx_id = create_tx(&server, &token1, acc, "Income", "50").await;

    let res = server
        .get(&format!("/transactions/{tx_id}"))
        .add_header(helpers::auth(&token2).0, helpers::auth(&token2).1)
        .await;

    assert_eq!(res.status_code(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn delete_transaction_returns_204() {
    let postgres = common::TestPostgres::new().await;
    let server = helpers::make_app(postgres.pool).await;
    let (_uid, token) = helpers::create_test_user();
    let account_id = helpers::create_account_for(&server, &token).await;
    let tx_id = create_tx(&server, &token, account_id, "Income", "10").await;

    let res = server
        .delete(&format!("/transactions/{tx_id}"))
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .await;

    assert_eq!(res.status_code(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn delete_transaction_not_found_returns_404() {
    let postgres = common::TestPostgres::new().await;
    let server = helpers::make_app(postgres.pool).await;
    let (_uid, token) = helpers::create_test_user();
    let random_id = Uuid::new_v4();

    let res = server
        .delete(&format!("/transactions/{random_id}"))
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .await;

    assert_eq!(res.status_code(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn balance_updates_after_income_and_expense() {
    let postgres = common::TestPostgres::new().await;
    let server = helpers::make_app(postgres.pool).await;
    let (_uid, token) = helpers::create_test_user();
    let account_id = helpers::create_account_for(&server, &token).await;

    create_tx(&server, &token, account_id, "Income", "100").await;
    create_tx(&server, &token, account_id, "Expense", "30").await;

    let res = server
        .get(&format!("/accounts/{account_id}/balance"))
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .await;

    let body: Value = res.json();
    assert_eq!(body["balance"], "70");
}
