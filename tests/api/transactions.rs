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
    create_tx_at(
        server,
        token,
        account_id,
        kind,
        amount,
        "2024-01-01T00:00:00Z",
    )
    .await
}

async fn create_tx_at(
    server: &axum_test::TestServer,
    token: &str,
    account_id: Uuid,
    kind: &str,
    amount: &str,
    transacted_at: &str,
) -> Uuid {
    let res = server
        .post(&format!("/accounts/{account_id}/transactions"))
        .add_header(helpers::auth(token).0, helpers::auth(token).1)
        .json(&serde_json::json!({
            "amount": amount,
            "currency": "USD",
            "kind": kind,
            "transacted_at": transacted_at
        }))
        .await;
    Uuid::parse_str(res.json::<Value>()["id"].as_str().unwrap()).unwrap()
}

async fn create_tx_with_category(
    server: &axum_test::TestServer,
    token: &str,
    account_id: Uuid,
    kind: &str,
    amount: &str,
    category_id: Uuid,
) -> Uuid {
    let res = server
        .post(&format!("/accounts/{account_id}/transactions"))
        .add_header(helpers::auth(token).0, helpers::auth(token).1)
        .json(&serde_json::json!({
            "amount": amount,
            "currency": "USD",
            "kind": kind,
            "category_id": category_id,
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
    assert_eq!(body["items"].as_array().unwrap().len(), 2);
    assert_eq!(body["pagination"]["total"], 2);
    assert_eq!(body["pagination"]["limit"], 50);
    assert_eq!(body["pagination"]["offset"], 0);
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
    let items = body["items"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert!(items.iter().all(|t| t["kind"] == "Income"));
    assert_eq!(body["pagination"]["total"], 2);
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
    let body1: Value = res_page1.json();
    assert_eq!(body1["items"].as_array().unwrap().len(), 1);
    assert_eq!(body1["pagination"]["total"], 3);
    assert_eq!(body1["pagination"]["limit"], 1);
    assert_eq!(body1["pagination"]["offset"], 0);

    let res_page2 = server
        .get(&format!(
            "/accounts/{account_id}/transactions?limit=1&offset=1"
        ))
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .await;
    let body2: Value = res_page2.json();
    assert_eq!(body2["items"].as_array().unwrap().len(), 1);
    assert_eq!(body2["pagination"]["total"], 3);
    assert_eq!(body2["pagination"]["offset"], 1);
}

#[tokio::test]
async fn list_account_transactions_total_reflects_full_count() {
    let postgres = common::TestPostgres::new().await;
    let server = helpers::make_app(postgres.pool).await;
    let (_uid, token) = helpers::create_test_user();
    let account_id = helpers::create_account_for(&server, &token).await;

    for i in 1..=5 {
        create_tx(&server, &token, account_id, "Income", &i.to_string()).await;
    }

    let res = server
        .get(&format!(
            "/accounts/{account_id}/transactions?limit=2&offset=0"
        ))
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .await;

    let body: Value = res.json();
    assert_eq!(body["items"].as_array().unwrap().len(), 2);
    assert_eq!(body["pagination"]["total"], 5);
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
    assert_eq!(body["items"].as_array().unwrap().len(), 2);
    assert_eq!(body["pagination"]["total"], 2);
}

#[tokio::test]
async fn list_all_transactions_no_auth_returns_401() {
    let postgres = common::TestPostgres::new().await;
    let server = helpers::make_app(postgres.pool).await;

    let res = server.get("/transactions").await;

    assert_eq!(res.status_code(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn list_account_transactions_filter_by_date_range() {
    let postgres = common::TestPostgres::new().await;
    let server = helpers::make_app(postgres.pool).await;
    let (_uid, token) = helpers::create_test_user();
    let account_id = helpers::create_account_for(&server, &token).await;

    create_tx_at(
        &server,
        &token,
        account_id,
        "Income",
        "1",
        "2024-01-01T00:00:00Z",
    )
    .await;
    let mid = create_tx_at(
        &server,
        &token,
        account_id,
        "Income",
        "2",
        "2024-06-01T00:00:00Z",
    )
    .await;
    create_tx_at(
        &server,
        &token,
        account_id,
        "Income",
        "3",
        "2024-12-01T00:00:00Z",
    )
    .await;

    // 2024-04-01 .. 2024-08-01 (only the middle tx)
    let from: i64 = 1711929600;
    let to: i64 = 1722470400;

    let res = server
        .get(&format!(
            "/accounts/{account_id}/transactions?from={from}&to={to}"
        ))
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .await;

    assert_eq!(res.status_code(), StatusCode::OK);
    let body: Value = res.json();
    let items = body["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["id"].as_str().unwrap(), mid.to_string());
    assert_eq!(body["pagination"]["total"], 1);
}

#[tokio::test]
async fn list_account_transactions_filter_by_category() {
    let postgres = common::TestPostgres::new().await;
    let server = helpers::make_app(postgres.pool).await;
    let (_uid, token) = helpers::create_test_user();
    let account_id = helpers::create_account_for(&server, &token).await;
    let cat_id = helpers::create_category_for(&server, &token).await;

    let categorized =
        create_tx_with_category(&server, &token, account_id, "Expense", "10", cat_id).await;
    create_tx(&server, &token, account_id, "Expense", "20").await;

    let res = server
        .get(&format!(
            "/accounts/{account_id}/transactions?category_id={cat_id}"
        ))
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .await;

    let body: Value = res.json();
    let items = body["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["id"].as_str().unwrap(), categorized.to_string());
    assert_eq!(body["pagination"]["total"], 1);
}

#[tokio::test]
async fn list_account_transactions_invalid_kind_returns_400() {
    let postgres = common::TestPostgres::new().await;
    let server = helpers::make_app(postgres.pool).await;
    let (_uid, token) = helpers::create_test_user();
    let account_id = helpers::create_account_for(&server, &token).await;

    let res = server
        .get(&format!("/accounts/{account_id}/transactions?kind=Bogus"))
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .await;

    assert_eq!(res.status_code(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn list_account_transactions_no_auth_returns_401() {
    let postgres = common::TestPostgres::new().await;
    let server = helpers::make_app(postgres.pool).await;
    let (_uid, token) = helpers::create_test_user();
    let account_id = helpers::create_account_for(&server, &token).await;

    let res = server
        .get(&format!("/accounts/{account_id}/transactions"))
        .await;

    assert_eq!(res.status_code(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn list_account_transactions_other_users_account_returns_empty() {
    let postgres = common::TestPostgres::new().await;
    let server = helpers::make_app(postgres.pool).await;
    let (_uid_a, token_a) = helpers::create_test_user();
    let (_uid_b, token_b) = helpers::create_test_user();

    let acc_a = helpers::create_account_for(&server, &token_a).await;
    create_tx(&server, &token_a, acc_a, "Income", "100").await;

    // user B requests user A's account path. Current behavior: empty list, not 404.
    let res = server
        .get(&format!("/accounts/{acc_a}/transactions"))
        .add_header(helpers::auth(&token_b).0, helpers::auth(&token_b).1)
        .await;

    assert_eq!(res.status_code(), StatusCode::OK);
    let body: Value = res.json();
    assert_eq!(body["items"].as_array().unwrap().len(), 0);
    assert_eq!(body["pagination"]["total"], 0);
}

#[tokio::test]
async fn list_account_transactions_unknown_account_returns_empty() {
    let postgres = common::TestPostgres::new().await;
    let server = helpers::make_app(postgres.pool).await;
    let (_uid, token) = helpers::create_test_user();
    let random_id = Uuid::new_v4();

    // No ownership/existence check on the path; SQL filter just yields empty.
    let res = server
        .get(&format!("/accounts/{random_id}/transactions"))
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .await;

    assert_eq!(res.status_code(), StatusCode::OK);
    let body: Value = res.json();
    assert_eq!(body["items"].as_array().unwrap().len(), 0);
    assert_eq!(body["pagination"]["total"], 0);
}

#[tokio::test]
async fn list_all_transactions_filter_by_date_range() {
    let postgres = common::TestPostgres::new().await;
    let server = helpers::make_app(postgres.pool).await;
    let (_uid, token) = helpers::create_test_user();
    let acc1 = helpers::create_account_for(&server, &token).await;
    let acc2 = helpers::create_account_for(&server, &token).await;

    create_tx_at(&server, &token, acc1, "Income", "1", "2024-01-01T00:00:00Z").await;
    let mid = create_tx_at(
        &server,
        &token,
        acc2,
        "Expense",
        "2",
        "2024-06-01T00:00:00Z",
    )
    .await;
    create_tx_at(&server, &token, acc1, "Income", "3", "2024-12-01T00:00:00Z").await;

    let from: i64 = 1711929600;
    let to: i64 = 1722470400;

    let res = server
        .get(&format!("/transactions?from={from}&to={to}"))
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .await;

    let body: Value = res.json();
    let items = body["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["id"].as_str().unwrap(), mid.to_string());
    assert_eq!(body["pagination"]["total"], 1);
}

#[tokio::test]
async fn list_all_transactions_pagination() {
    let postgres = common::TestPostgres::new().await;
    let server = helpers::make_app(postgres.pool).await;
    let (_uid, token) = helpers::create_test_user();
    let acc1 = helpers::create_account_for(&server, &token).await;
    let acc2 = helpers::create_account_for(&server, &token).await;

    for i in 0..5 {
        let acc = if i % 2 == 0 { acc1 } else { acc2 };
        let when = format!("2024-0{}-01T00:00:00Z", i + 1);
        create_tx_at(&server, &token, acc, "Income", &(i + 1).to_string(), &when).await;
    }

    let mut seen = std::collections::HashSet::new();
    for offset in [0, 2, 4] {
        let res = server
            .get(&format!("/transactions?limit=2&offset={offset}"))
            .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
            .await;
        let body: Value = res.json();
        assert_eq!(body["pagination"]["total"], 5);
        for item in body["items"].as_array().unwrap() {
            assert!(seen.insert(item["id"].as_str().unwrap().to_string()));
        }
    }
    assert_eq!(seen.len(), 5);
}

#[tokio::test]
async fn list_all_transactions_empty_when_no_data() {
    let postgres = common::TestPostgres::new().await;
    let server = helpers::make_app(postgres.pool).await;
    let (_uid, token) = helpers::create_test_user();

    let res = server
        .get("/transactions")
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .await;

    assert_eq!(res.status_code(), StatusCode::OK);
    let body: Value = res.json();
    assert_eq!(body["items"].as_array().unwrap().len(), 0);
    assert_eq!(body["pagination"]["total"], 0);
}

#[tokio::test]
async fn list_account_transactions_out_of_range_timestamp_returns_400() {
    let postgres = common::TestPostgres::new().await;
    let server = helpers::make_app(postgres.pool).await;
    let (_uid, token) = helpers::create_test_user();
    let account_id = helpers::create_account_for(&server, &token).await;

    let res = server
        .get(&format!(
            "/accounts/{account_id}/transactions?from={}",
            i64::MAX
        ))
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .await;

    assert_eq!(res.status_code(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn list_account_transactions_inverted_range_returns_400() {
    let postgres = common::TestPostgres::new().await;
    let server = helpers::make_app(postgres.pool).await;
    let (_uid, token) = helpers::create_test_user();
    let account_id = helpers::create_account_for(&server, &token).await;

    // from > to
    let from: i64 = 1722470400; // 2024-08-01
    let to: i64 = 1711929600; // 2024-04-01
    let res = server
        .get(&format!(
            "/accounts/{account_id}/transactions?from={from}&to={to}"
        ))
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .await;

    assert_eq!(res.status_code(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn list_all_transactions_out_of_range_timestamp_returns_400() {
    let postgres = common::TestPostgres::new().await;
    let server = helpers::make_app(postgres.pool).await;
    let (_uid, token) = helpers::create_test_user();

    let res = server
        .get(&format!("/transactions?to={}", i64::MIN))
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .await;

    assert_eq!(res.status_code(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn list_all_transactions_inverted_range_returns_400() {
    let postgres = common::TestPostgres::new().await;
    let server = helpers::make_app(postgres.pool).await;
    let (_uid, token) = helpers::create_test_user();

    let from: i64 = 1722470400;
    let to: i64 = 1711929600;
    let res = server
        .get(&format!("/transactions?from={from}&to={to}"))
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .await;

    assert_eq!(res.status_code(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn list_all_transactions_only_returns_users_own() {
    let postgres = common::TestPostgres::new().await;
    let server = helpers::make_app(postgres.pool).await;
    let (_uid_a, token_a) = helpers::create_test_user();
    let (_uid_b, token_b) = helpers::create_test_user();

    let acc_a = helpers::create_account_for(&server, &token_a).await;
    let acc_b = helpers::create_account_for(&server, &token_b).await;
    let tx_a = create_tx(&server, &token_a, acc_a, "Income", "10").await;
    let tx_b = create_tx(&server, &token_b, acc_b, "Income", "20").await;

    let res_a = server
        .get("/transactions")
        .add_header(helpers::auth(&token_a).0, helpers::auth(&token_a).1)
        .await;
    let body_a: Value = res_a.json();
    let items_a = body_a["items"].as_array().unwrap();
    assert_eq!(items_a.len(), 1);
    assert_eq!(items_a[0]["id"].as_str().unwrap(), tx_a.to_string());

    let res_b = server
        .get("/transactions")
        .add_header(helpers::auth(&token_b).0, helpers::auth(&token_b).1)
        .await;
    let body_b: Value = res_b.json();
    let items_b = body_b["items"].as_array().unwrap();
    assert_eq!(items_b.len(), 1);
    assert_eq!(items_b[0]["id"].as_str().unwrap(), tx_b.to_string());
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

async fn connect_mono_for_account(
    server: &axum_test::TestServer,
    token: &str,
    account_id: Uuid,
    mono_account_id: &str,
) {
    server
        .post("/monobank/connect")
        .add_header(helpers::auth(token).0, helpers::auth(token).1)
        .json(&serde_json::json!({
            "account_id": account_id,
            "token": "fake-mono-token",
            "external_account_id": mono_account_id
        }))
        .await;
}

#[tokio::test]
async fn create_transaction_rejected_on_bank_linked_account_returns_409() {
    let postgres = common::TestPostgres::new().await;
    let server = helpers::make_app(postgres.pool).await;
    let (_uid, token) = helpers::create_test_user();
    let account_id = helpers::create_account_for(&server, &token).await;
    connect_mono_for_account(&server, &token, account_id, "mono-reject-1").await;

    let res = server
        .post(&format!("/accounts/{account_id}/transactions"))
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .json(&serde_json::json!({
            "amount": "10",
            "currency": "USD",
            "kind": "Expense",
            "transacted_at": "2024-01-01T00:00:00Z"
        }))
        .await;

    assert_eq!(res.status_code(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn delete_transaction_rejected_on_bank_linked_account_returns_409() {
    let postgres = common::TestPostgres::new().await;
    let server = helpers::make_app(postgres.pool).await;
    let (_uid, token) = helpers::create_test_user();
    let account_id = helpers::create_account_for(&server, &token).await;

    // First insert a normal manual transaction (before linking the connection),
    // then link the account to Monobank. Deletion attempts must then fail.
    let tx_id = create_tx(&server, &token, account_id, "Income", "10").await;
    connect_mono_for_account(&server, &token, account_id, "mono-reject-2").await;

    let res = server
        .delete(&format!("/transactions/{tx_id}"))
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .await;

    assert_eq!(res.status_code(), StatusCode::CONFLICT);
}
