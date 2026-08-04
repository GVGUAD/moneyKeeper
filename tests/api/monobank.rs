use axum::http::StatusCode;
use serde_json::Value;
use std::sync::Arc;

use super::{common, helpers};

/// Helper: creates a monobank connection for the given account.
/// Returns `connection_id_str`.
async fn connect_mono(
    server: &axum_test::TestServer,
    token: &str,
    account_id: uuid::Uuid,
    mono_account_id: &str,
) -> String {
    let res = server
        .post("/monobank/connect")
        .add_header(helpers::auth(token).0, helpers::auth(token).1)
        .json(&serde_json::json!({
            "account_id": account_id,
            "token": "fake-mono-token",
            "external_account_id": mono_account_id
        }))
        .await;
    let body: Value = res.json();
    body["id"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn connect_returns_201_with_pending_status() {
    let postgres = common::TestPostgres::new().await;
    let server = helpers::make_app(postgres.pool).await;
    let (_uid, token) = helpers::create_test_user();
    let account_id = helpers::create_account_for(&server, &token).await;

    let res = server
        .post("/monobank/connect")
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .json(&serde_json::json!({
            "account_id": account_id,
            "token": "fake-mono-token",
            "external_account_id": "mono-card-1"
        }))
        .await;

    assert_eq!(res.status_code(), StatusCode::CREATED);
    let body: Value = res.json();
    assert_eq!(body["sync_status"], "pending");
    assert!(body["id"].is_string());
}

#[tokio::test]
async fn list_connections_returns_created_connection() {
    let postgres = common::TestPostgres::new().await;
    let server = helpers::make_app(postgres.pool).await;
    let (_uid, token) = helpers::create_test_user();
    let account_id = helpers::create_account_for(&server, &token).await;

    connect_mono(&server, &token, account_id, "mono-card-2").await;

    let res = server
        .get("/monobank/connections")
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .await;

    assert_eq!(res.status_code(), StatusCode::OK);
    let body: Value = res.json();
    let arr = body.as_array().unwrap();
    assert_eq!(arr.len(), 1);
}

#[tokio::test]
async fn delete_connection_returns_204() {
    let postgres = common::TestPostgres::new().await;
    let server = helpers::make_app(postgres.pool).await;
    let (_uid, token) = helpers::create_test_user();
    let account_id = helpers::create_account_for(&server, &token).await;

    let conn_id = connect_mono(&server, &token, account_id, "mono-card-3").await;

    let res = server
        .delete(&format!("/monobank/connections/{conn_id}"))
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .await;

    assert_eq!(res.status_code(), StatusCode::NO_CONTENT);

    let list_res = server
        .get("/monobank/connections")
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .await;

    let body: Value = list_res.json();
    assert_eq!(body.as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn webhook_inserts_expense_transaction() {
    let postgres = common::TestPostgres::new().await;
    let server = helpers::make_app(postgres.pool).await;
    let (_uid, token) = helpers::create_test_user();
    let account_id = helpers::create_account_for(&server, &token).await;

    connect_mono(&server, &token, account_id, "mono-card-4").await;

    let webhook_payload = serde_json::json!({
        "type": "StatementItem",
        "data": {
            "account": "mono-card-4",
            "statementItem": {
                "id": "stmt-unique-1",
                "time": 1700000000_i64,
                "description": "Coffee",
                "mcc": 5812,
                "amount": -5000_i64,
                "operationAmount": -5000_i64,
                "currencyCode": 980,
                "balance": 100000_i64,
                "hold": false
            }
        }
    });

    let res = server
        .post("/monobank/webhook")
        .json(&webhook_payload)
        .await;
    assert_eq!(res.status_code(), StatusCode::OK);

    let txn_res = server
        .get(&format!("/accounts/{account_id}/transactions"))
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .await;

    assert_eq!(txn_res.status_code(), StatusCode::OK);
    let body: Value = txn_res.json();
    let txns = body["items"].as_array().unwrap();
    assert_eq!(txns.len(), 1);
    assert_eq!(txns[0]["kind"], "Expense");
}

#[tokio::test]
async fn webhook_duplicate_is_silently_ignored() {
    let postgres = common::TestPostgres::new().await;
    let server = helpers::make_app(postgres.pool).await;
    let (_uid, token) = helpers::create_test_user();
    let account_id = helpers::create_account_for(&server, &token).await;

    connect_mono(&server, &token, account_id, "mono-card-5").await;

    let webhook_payload = serde_json::json!({
        "type": "StatementItem",
        "data": {
            "account": "mono-card-5",
            "statementItem": {
                "id": "stmt-unique-dup",
                "time": 1700000001_i64,
                "description": "Duplicate",
                "mcc": 5812,
                "amount": -3000_i64,
                "operationAmount": -3000_i64,
                "currencyCode": 980,
                "balance": 97000_i64,
                "hold": false
            }
        }
    });

    // Send the same webhook twice
    server
        .post("/monobank/webhook")
        .json(&webhook_payload)
        .await;
    let res2 = server
        .post("/monobank/webhook")
        .json(&webhook_payload)
        .await;
    assert_eq!(res2.status_code(), StatusCode::OK);

    let txn_res = server
        .get(&format!("/accounts/{account_id}/transactions"))
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .await;

    assert_eq!(txn_res.status_code(), StatusCode::OK);
    let body: Value = txn_res.json();
    let txns = body["items"].as_array().unwrap();
    assert_eq!(txns.len(), 1);
}

async fn wait_until_completed(server: &axum_test::TestServer, token: &str, conn_id: &str) {
    for _ in 0..50 {
        let res = server
            .get("/monobank/connections")
            .add_header(helpers::auth(token).0, helpers::auth(token).1)
            .await;
        let body: Value = res.json();
        let found = body
            .as_array()
            .and_then(|arr| arr.iter().find(|c| c["id"] == conn_id))
            .cloned();
        if let Some(c) = found
            && c["sync_status"] == "completed"
        {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!("connection {conn_id} did not reach sync_status=completed in time");
}

#[tokio::test]
async fn resync_returns_202_with_job_response() {
    let postgres = common::TestPostgres::new().await;
    let server = helpers::make_app(postgres.pool).await;
    let (_uid, token) = helpers::create_test_user();
    let account_id = helpers::create_account_for(&server, &token).await;

    let conn_id = connect_mono(&server, &token, account_id, "mono-resync-1").await;
    wait_until_completed(&server, &token, &conn_id).await;

    let res = server
        .post(&format!(
            "/monobank/connections/{conn_id}/resync?from=1700000000&to=1700500000"
        ))
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .await;

    assert_eq!(res.status_code(), StatusCode::ACCEPTED);
    let body: Value = res.json();
    assert_eq!(body["connection_id"], conn_id);
    assert_eq!(body["sync_status"], "syncing");
    assert_eq!(body["from"], 1_700_000_000_i64);
    assert_eq!(body["to"], 1_700_500_000_i64);
    assert!(body["enqueued_at"].is_string());
}

#[tokio::test]
async fn resync_rejects_to_less_than_from() {
    let postgres = common::TestPostgres::new().await;
    let server = helpers::make_app(postgres.pool).await;
    let (_uid, token) = helpers::create_test_user();
    let account_id = helpers::create_account_for(&server, &token).await;
    let conn_id = connect_mono(&server, &token, account_id, "mono-resync-2").await;
    wait_until_completed(&server, &token, &conn_id).await;

    let res = server
        .post(&format!(
            "/monobank/connections/{conn_id}/resync?from=1700000100&to=1700000000"
        ))
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .await;

    assert_eq!(res.status_code(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn resync_rejects_negative_epoch() {
    let postgres = common::TestPostgres::new().await;
    let server = helpers::make_app(postgres.pool).await;
    let (_uid, token) = helpers::create_test_user();
    let account_id = helpers::create_account_for(&server, &token).await;
    let conn_id = connect_mono(&server, &token, account_id, "mono-resync-3").await;
    wait_until_completed(&server, &token, &conn_id).await;

    let res = server
        .post(&format!(
            "/monobank/connections/{conn_id}/resync?from=-1&to=1700000000"
        ))
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .await;

    assert_eq!(res.status_code(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn resync_rejects_missing_params() {
    let postgres = common::TestPostgres::new().await;
    let server = helpers::make_app(postgres.pool).await;
    let (_uid, token) = helpers::create_test_user();
    let account_id = helpers::create_account_for(&server, &token).await;
    let conn_id = connect_mono(&server, &token, account_id, "mono-resync-4").await;
    wait_until_completed(&server, &token, &conn_id).await;

    let res = server
        .post(&format!("/monobank/connections/{conn_id}/resync?from=1"))
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .await;

    assert_eq!(res.status_code(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn resync_unknown_connection_returns_404() {
    let postgres = common::TestPostgres::new().await;
    let server = helpers::make_app(postgres.pool).await;
    let (_uid, token) = helpers::create_test_user();

    let unknown = uuid::Uuid::new_v4();
    let res = server
        .post(&format!(
            "/monobank/connections/{unknown}/resync?from=0&to=1700000000"
        ))
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .await;

    assert_eq!(res.status_code(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn resync_other_users_connection_returns_404() {
    let postgres = common::TestPostgres::new().await;
    let server = helpers::make_app(postgres.pool).await;
    let (_uid_a, token_a) = helpers::create_test_user();
    let (_uid_b, token_b) = helpers::create_test_user();
    let account_id = helpers::create_account_for(&server, &token_a).await;
    let conn_id = connect_mono(&server, &token_a, account_id, "mono-resync-5").await;
    wait_until_completed(&server, &token_a, &conn_id).await;

    let res = server
        .post(&format!(
            "/monobank/connections/{conn_id}/resync?from=0&to=1700000000"
        ))
        .add_header(helpers::auth(&token_b).0, helpers::auth(&token_b).1)
        .await;

    assert_eq!(res.status_code(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn resync_conflict_when_already_syncing() {
    let postgres = common::TestPostgres::new().await;
    let pool = postgres.pool.clone();
    let server = helpers::make_app(postgres.pool).await;
    let (_uid, token) = helpers::create_test_user();
    let account_id = helpers::create_account_for(&server, &token).await;
    let conn_id_str = connect_mono(&server, &token, account_id, "mono-resync-6").await;
    wait_until_completed(&server, &token, &conn_id_str).await;

    let conn_uuid = uuid::Uuid::parse_str(&conn_id_str).unwrap();
    sqlx::query("UPDATE bank_connections SET sync_status = 'syncing' WHERE id = $1")
        .bind(conn_uuid)
        .execute(&pool)
        .await
        .unwrap();

    let res = server
        .post(&format!(
            "/monobank/connections/{conn_id_str}/resync?from=0&to=1700000000"
        ))
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .await;

    assert_eq!(res.status_code(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn get_client_info_proxies_to_monobank() {
    let postgres = common::TestPostgres::new().await;
    let mock_accounts = vec![moneykeeper::domain::monobank::MonoAccount {
        id: "acc-1".into(),
        currency_code: 980,
        balance: 50000,
        credit_limit: 0,
        account_type: "black".into(),
        iban: Some("UA123456789".into()),
    }];

    let client = Arc::new(helpers::MockMonobankClient {
        accounts: mock_accounts,
        statement_items: vec![],
    });
    let server = helpers::make_app_with_client(postgres.pool, client).await;
    let (_uid, token) = helpers::create_test_user();

    let res = server
        .get("/monobank/client-info")
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .add_header(
            axum::http::HeaderName::from_static("x-token"),
            "any-token".parse::<axum::http::HeaderValue>().unwrap(),
        )
        .await;

    assert_eq!(res.status_code(), StatusCode::OK);
    let body: Value = res.json();
    let accounts = body.as_array().unwrap();
    assert!(!accounts.is_empty());
    assert_eq!(accounts[0]["id"], "acc-1");
}
