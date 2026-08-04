use axum::http::StatusCode;
use chrono::Utc;
use rust_decimal_macros::dec;
use serde_json::Value;
use uuid::Uuid;

use super::{common, helpers};

use moneykeeper::domain::subscription::{
    BillingPeriod, Subscription, SubscriptionProvider, SubscriptionStatus,
};
use moneykeeper::domain::subscription_charge::{
    ChargeMatchSource, ChargeMatchStatus, ChargeSource, ReceiptKind, SubscriptionCharge,
};

/// Insert a subscription + one pending charge directly via the service repos.
/// Returns `(subscription_id, charge_id)`.
async fn seed_subscription_and_charge(ctx: &helpers::TestContext, user_id: Uuid) -> (Uuid, Uuid) {
    let sub = Subscription {
        id: Uuid::new_v4(),
        user_id,
        provider: SubscriptionProvider::Netflix,
        product_name: "Netflix Premium".into(),
        merchant_key: format!("netflix.com:premium-test-{}", Uuid::new_v4()),
        amount: dec!(15.99),
        currency: "USD".into(),
        billing_period: BillingPeriod::Monthly,
        status: SubscriptionStatus::Active,
        started_at: Utc::now(),
        last_charged_at: None,
        next_expected_at: None,
        category_id: None,
        overrides: Default::default(),
        created_at: Utc::now(),
    };
    let inserted_sub = ctx
        .subscription_repo
        .upsert_by_merchant_key(&sub)
        .await
        .expect("upsert subscription");

    let source_key = format!("other:test:{}", Uuid::new_v4());
    let charge = SubscriptionCharge {
        id: Uuid::new_v4(),
        subscription_id: inserted_sub.id,
        user_id,
        amount: dec!(15.99),
        currency: "USD".into(),
        charged_at: Utc::now(),
        email_message_id: source_key.clone(),
        rfc_message_id: None,
        source: ChargeSource::Other,
        source_key,
        source_connection_id: None,
        provider_message_id: None,
        kind: ReceiptKind::Renewal,
        transaction_id: None,
        match_status: ChargeMatchStatus::Pending,
        match_started_at: Utc::now(),
        match_source: None,
        created_at: Utc::now(),
    };
    let (charge_id, _) = ctx
        .charge_repo
        .create_idempotent(&charge)
        .await
        .expect("create charge");

    (inserted_sub.id, charge_id)
}

async fn create_transaction(
    ctx: &helpers::TestContext,
    token: &str,
    account_id: Uuid,
    kind: &str,
    category_id: Option<Uuid>,
    transacted_at: &str,
) -> Uuid {
    let response = ctx
        .server
        .post(&format!("/accounts/{account_id}/transactions"))
        .add_header(helpers::auth(token).0, helpers::auth(token).1)
        .json(&serde_json::json!({
            "amount": "19.99",
            "currency": "usd",
            "kind": kind,
            "category_id": category_id,
            "transacted_at": transacted_at
        }))
        .await;
    assert_eq!(response.status_code(), StatusCode::CREATED);
    Uuid::parse_str(response.json::<Value>()["id"].as_str().unwrap()).unwrap()
}

#[tokio::test]
async fn list_subscriptions_empty_for_new_user() {
    let postgres = common::TestPostgres::new().await;
    let ctx = helpers::make_app_ctx(postgres.pool).await;
    let (_uid, token) = helpers::create_test_user();

    let res = ctx
        .server
        .get("/subscriptions")
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .await;

    assert_eq!(res.status_code(), StatusCode::OK);
    let body: Value = res.json();
    assert_eq!(body.as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn list_subscriptions_returns_inserted_subscription() {
    let postgres = common::TestPostgres::new().await;
    let ctx = helpers::make_app_ctx(postgres.pool).await;
    let (user_id, token) = helpers::create_test_user();

    let (sub_id, _charge_id) = seed_subscription_and_charge(&ctx, user_id).await;

    let res = ctx
        .server
        .get("/subscriptions")
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .await;

    assert_eq!(res.status_code(), StatusCode::OK);
    let body: Value = res.json();
    let items = body.as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["id"].as_str().unwrap(), sub_id.to_string());
    assert_eq!(items[0]["provider"], "netflix");
    assert_eq!(items[0]["product_name"], "Netflix Premium");
    assert_eq!(items[0]["status"], "active");
}

#[tokio::test]
async fn get_subscription_returns_correct_data() {
    let postgres = common::TestPostgres::new().await;
    let ctx = helpers::make_app_ctx(postgres.pool).await;
    let (user_id, token) = helpers::create_test_user();

    let (sub_id, charge_id) = seed_subscription_and_charge(&ctx, user_id).await;

    let res = ctx
        .server
        .get(&format!("/subscriptions/{sub_id}"))
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .await;

    assert_eq!(res.status_code(), StatusCode::OK);
    let body: Value = res.json();
    assert_eq!(body["id"].as_str().unwrap(), sub_id.to_string());
    assert_eq!(body["currency"], "USD");
    assert_eq!(body["amount"], "15.99");
    assert_eq!(body["billing_period"], "monthly");
    assert_eq!(body["overrides"]["product_name"], Value::Null);
    let charges = body["charges"].as_array().expect("detail charges");
    assert_eq!(charges.len(), 1);
    assert_eq!(charges[0]["id"], charge_id.to_string());
}

#[tokio::test]
async fn get_subscription_unknown_returns_404() {
    let postgres = common::TestPostgres::new().await;
    let ctx = helpers::make_app_ctx(postgres.pool).await;
    let (_uid, token) = helpers::create_test_user();

    let res = ctx
        .server
        .get(&format!("/subscriptions/{}", Uuid::new_v4()))
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .await;

    assert_eq!(res.status_code(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn list_charges_returns_pending_charge() {
    let postgres = common::TestPostgres::new().await;
    let ctx = helpers::make_app_ctx(postgres.pool).await;
    let (user_id, token) = helpers::create_test_user();

    let (sub_id, charge_id) = seed_subscription_and_charge(&ctx, user_id).await;

    let res = ctx
        .server
        .get(&format!("/subscriptions/{sub_id}/charges"))
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .await;

    assert_eq!(res.status_code(), StatusCode::OK);
    let body: Value = res.json();
    let charges = body.as_array().unwrap();
    assert_eq!(charges.len(), 1);
    assert_eq!(charges[0]["id"].as_str().unwrap(), charge_id.to_string());
    assert_eq!(charges[0]["match_status"], "Pending");
    assert_eq!(charges[0]["amount"], "15.99");
}

#[tokio::test]
async fn forecast_returns_non_zero_for_active_subscription() {
    let postgres = common::TestPostgres::new().await;
    let pool = postgres.pool.clone();

    // Seed a USD→UAH rate for today so the forecast can convert USD→UAH
    // (default base_currency is UAH).
    helpers::seed_fx_rate(
        &postgres.pool,
        chrono::Utc::now().date_naive(),
        "USD",
        rust_decimal::Decimal::new(40, 0), // 40 UAH per USD
    )
    .await;

    let ctx = helpers::make_app_ctx(postgres.pool).await;
    let (user_id, token) = helpers::create_test_user();

    let (sub_id, _) = seed_subscription_and_charge(&ctx, user_id).await;
    sqlx::query("UPDATE subscriptions SET next_expected_at = $1 WHERE id = $2")
        .bind((Utc::now() + chrono::Duration::days(1)).timestamp())
        .bind(sub_id)
        .execute(&pool)
        .await
        .expect("schedule forecast occurrence");

    let res = ctx
        .server
        .get("/subscriptions/forecast")
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .await;

    assert_eq!(res.status_code(), StatusCode::OK);
    let body: Value = res.json();
    // Default base_currency is UAH.
    assert_eq!(body["base_currency"], "UAH");
    // 15.99 USD × 40 UAH/USD = 639.60 UAH — just verify it's > 0.
    let base_total: f64 = body["base_total"]
        .as_str()
        .unwrap()
        .parse()
        .expect("base_total should be a decimal string");
    assert!(base_total > 0.0, "base_total should be positive");
    assert!(body["window_start"].is_string());
    assert!(body["window_end"].is_string());
    assert!(body["monthly_equivalent_total"].is_string());
    assert!(body["yearly_equivalent_total"].is_string());
    assert!(body["normalized_by_currency"]["USD"]["monthly"].is_string());
    assert_eq!(body["complete"], true);
    assert_eq!(
        body["fx_quotes"][0]["rate_date"],
        Utc::now().date_naive().to_string()
    );
}

#[tokio::test]
async fn matcher_links_charge_to_matching_expense() {
    let postgres = common::TestPostgres::new().await;
    let ctx = helpers::make_app_ctx(postgres.pool).await;
    let (user_id, token) = helpers::create_test_user();
    let account_id = helpers::create_account_for(&ctx.server, &token).await;

    let (sub_id, charge_id) = seed_subscription_and_charge(&ctx, user_id).await;

    // Create an expense transaction that matches the charge amount (~15.99 USD).
    let tx_res = ctx
        .server
        .post(&format!("/accounts/{account_id}/transactions"))
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .json(&serde_json::json!({
            "amount": "15.99",
            "currency": "USD",
            "kind": "Expense",
            "transacted_at": Utc::now().to_rfc3339()
        }))
        .await;
    assert_eq!(tx_res.status_code(), StatusCode::CREATED);

    // Run the matcher directly.
    ctx.matcher.run_for_user(user_id).await.expect("matcher");

    // The charge should now be Matched.
    let charges_res = ctx
        .server
        .get(&format!("/subscriptions/{sub_id}/charges"))
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .await;
    let charges_body: Value = charges_res.json();
    let charge = charges_body
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"].as_str().unwrap() == charge_id.to_string())
        .expect("charge not found");
    assert_eq!(charge["match_status"], "Matched");
    assert!(charge["transaction_id"].is_string());
}

#[tokio::test]
async fn manual_link_and_unlink_charge() {
    let postgres = common::TestPostgres::new().await;
    let pool = postgres.pool.clone();
    let ctx = helpers::make_app_ctx(postgres.pool).await;
    let (user_id, token) = helpers::create_test_user();
    let account_id = helpers::create_account_for(&ctx.server, &token).await;

    let (sub_id, charge_id) = seed_subscription_and_charge(&ctx, user_id).await;

    // Create an expense transaction.
    let tx_res = ctx
        .server
        .post(&format!("/accounts/{account_id}/transactions"))
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .json(&serde_json::json!({
            "amount": "15.99",
            "currency": "USD",
            "kind": "Expense",
            "transacted_at": "2024-05-01T00:00:00Z"
        }))
        .await;
    assert_eq!(tx_res.status_code(), StatusCode::CREATED);
    let tx_id = tx_res.json::<Value>()["id"].as_str().unwrap().to_string();

    // Manually link the charge to the transaction.
    let link_res = ctx
        .server
        .post(&format!("/subscription-charges/{charge_id}/link"))
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .json(&serde_json::json!({ "transaction_id": tx_id }))
        .await;
    assert_eq!(link_res.status_code(), StatusCode::NO_CONTENT);

    // Verify the charge is now Matched.
    let charges_res = ctx
        .server
        .get(&format!("/subscriptions/{sub_id}/charges"))
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .await;
    let charges_body: Value = charges_res.json();
    let charge = charges_body
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"].as_str().unwrap() == charge_id.to_string())
        .expect("charge not found");
    assert_eq!(charge["match_status"], "Matched");

    // Unlink the charge.
    let unlink_res = ctx
        .server
        .post(&format!("/subscription-charges/{charge_id}/unlink"))
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .await;
    assert_eq!(unlink_res.status_code(), StatusCode::NO_CONTENT);

    // Verify the charge is back to Pending.
    let charges_res2 = ctx
        .server
        .get(&format!("/subscriptions/{sub_id}/charges"))
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .await;
    let charges_body2: Value = charges_res2.json();
    let charge2 = charges_body2
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"].as_str().unwrap() == charge_id.to_string())
        .expect("charge not found");
    assert_eq!(charge2["match_status"], "Pending");
    assert!(charge2["transaction_id"].is_null());

    let rejected_pairs: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM subscription_charge_match_rejections WHERE charge_id = $1",
    )
    .bind(charge_id)
    .fetch_one(&pool)
    .await
    .expect("count rejected pairs");
    assert_eq!(rejected_pairs, 1);

    let already_unlinked = ctx
        .server
        .post(&format!("/subscription-charges/{charge_id}/unlink"))
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .await;
    assert_eq!(already_unlinked.status_code(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn delete_subscription_returns_204_and_removes_from_list() {
    let postgres = common::TestPostgres::new().await;
    let pool = postgres.pool.clone();
    let ctx = helpers::make_app_ctx(postgres.pool).await;
    let (user_id, token) = helpers::create_test_user();

    let (sub_id, _) = seed_subscription_and_charge(&ctx, user_id).await;

    let del_res = ctx
        .server
        .delete(&format!("/subscriptions/{sub_id}"))
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .await;
    assert_eq!(del_res.status_code(), StatusCode::NO_CONTENT);

    let list_res = ctx
        .server
        .get("/subscriptions")
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .await;
    assert_eq!(list_res.status_code(), StatusCode::OK);
    let body: Value = list_res.json();
    assert_eq!(body.as_array().unwrap().len(), 0);

    let tombstones: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM subscription_tombstones WHERE user_id = $1 AND provider = 'netflix'",
    )
    .bind(user_id)
    .fetch_one(&pool)
    .await
    .expect("count tombstones");
    assert_eq!(tombstones, 1);
}

#[tokio::test]
async fn subscriptions_require_auth() {
    let postgres = common::TestPostgres::new().await;
    let ctx = helpers::make_app_ctx(postgres.pool).await;

    let res = ctx.server.get("/subscriptions").await;
    assert_eq!(res.status_code(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn patch_subscription_sets_and_clears_durable_overrides() {
    let postgres = common::TestPostgres::new().await;
    let ctx = helpers::make_app_ctx(postgres.pool).await;
    let (user_id, token) = helpers::create_test_user();

    let (sub_id, _) = seed_subscription_and_charge(&ctx, user_id).await;

    let patch_res = ctx
        .server
        .patch(&format!("/subscriptions/{sub_id}"))
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .json(&serde_json::json!({
            "product_name": "Netflix Standard",
            "billing_period": "yearly",
            "status": "inactive"
        }))
        .await;
    assert_eq!(patch_res.status_code(), StatusCode::OK);
    let body: Value = patch_res.json();
    assert_eq!(body["product_name"], "Netflix Standard");
    assert_eq!(body["billing_period"], "yearly");
    assert_eq!(body["status"], "inactive");
    assert_eq!(body["overrides"]["product_name"], "Netflix Standard");
    assert_eq!(body["overrides"]["billing_period"], "yearly");
    assert_eq!(body["overrides"]["status"], "inactive");

    let clear_res = ctx
        .server
        .patch(&format!("/subscriptions/{sub_id}"))
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .json(&serde_json::json!({
            "product_name": null,
            "billing_period": null,
            "status": "auto"
        }))
        .await;
    assert_eq!(clear_res.status_code(), StatusCode::OK);
    let cleared: Value = clear_res.json();
    assert_eq!(cleared["product_name"], "Netflix Premium");
    assert_eq!(cleared["billing_period"], "monthly");
    assert_eq!(cleared["status"], "active");
    assert_eq!(cleared["overrides"]["product_name"], Value::Null);
    assert_eq!(cleared["overrides"]["billing_period"], Value::Null);
    assert_eq!(cleared["overrides"]["status"], Value::Null);
}

#[tokio::test]
async fn patch_subscription_validates_values_and_category_ownership() {
    let postgres = common::TestPostgres::new().await;
    let ctx = helpers::make_app_ctx(postgres.pool).await;
    let (user_id, token) = helpers::create_test_user();
    let (_other_user_id, other_token) = helpers::create_test_user();
    let (sub_id, _) = seed_subscription_and_charge(&ctx, user_id).await;
    let other_category = helpers::create_category_for(&ctx.server, &other_token).await;

    for payload in [
        serde_json::json!({ "billing_period": "daily" }),
        serde_json::json!({ "status": "paused" }),
        serde_json::json!({ "status": null }),
        serde_json::json!({ "product_name": "   " }),
        serde_json::json!({ "category_id": "not-a-uuid" }),
    ] {
        let response = ctx
            .server
            .patch(&format!("/subscriptions/{sub_id}"))
            .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
            .json(&payload)
            .await;
        assert_eq!(response.status_code(), StatusCode::BAD_REQUEST);
    }

    let response = ctx
        .server
        .patch(&format!("/subscriptions/{sub_id}"))
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .json(&serde_json::json!({ "category_id": other_category }))
        .await;
    assert_eq!(response.status_code(), StatusCode::NOT_FOUND);

    let (_, malformed_charge) = seed_subscription_and_charge(&ctx, user_id).await;
    let response = ctx
        .server
        .post(&format!("/subscription-charges/{malformed_charge}/link"))
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .json(&serde_json::json!({ "transaction_id": "not-a-uuid" }))
        .await;
    assert_eq!(response.status_code(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn one_transaction_cannot_be_linked_to_two_charges() {
    let postgres = common::TestPostgres::new().await;
    let ctx = helpers::make_app_ctx(postgres.pool).await;
    let (user_id, token) = helpers::create_test_user();
    let account_id = helpers::create_account_for(&ctx.server, &token).await;
    let (_, first_charge) = seed_subscription_and_charge(&ctx, user_id).await;
    let (_, second_charge) = seed_subscription_and_charge(&ctx, user_id).await;

    let tx_res = ctx
        .server
        .post(&format!("/accounts/{account_id}/transactions"))
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .json(&serde_json::json!({
            "amount": "15.99",
            "currency": "USD",
            "kind": "Expense",
            "transacted_at": Utc::now().to_rfc3339()
        }))
        .await;
    assert_eq!(tx_res.status_code(), StatusCode::CREATED);
    let tx_id = tx_res.json::<Value>()["id"].as_str().unwrap().to_string();

    let first = ctx
        .server
        .post(&format!("/subscription-charges/{first_charge}/link"))
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .json(&serde_json::json!({ "transaction_id": tx_id }))
        .await;
    assert_eq!(first.status_code(), StatusCode::NO_CONTENT);

    let second = ctx
        .server
        .post(&format!("/subscription-charges/{second_charge}/link"))
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .json(&serde_json::json!({ "transaction_id": tx_id }))
        .await;
    assert_eq!(second.status_code(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn monobank_webhook_triggers_cross_currency_subscription_matching() {
    let postgres = common::TestPostgres::new().await;
    let pool = postgres.pool.clone();
    helpers::seed_fx_rate(&pool, Utc::now().date_naive(), "USD", dec!(40)).await;
    let ctx = helpers::make_app_ctx(postgres.pool).await;
    let (user_id, token) = helpers::create_test_user();
    let account_id = helpers::create_account_for(&ctx.server, &token).await;
    let (_sub_id, charge_id) = seed_subscription_and_charge(&ctx, user_id).await;

    let connect = ctx
        .server
        .post("/monobank/connect")
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .json(&serde_json::json!({
            "account_id": account_id,
            "token": "fake-mono-token",
            "external_account_id": "mono-subscription-match"
        }))
        .await;
    assert_eq!(connect.status_code(), StatusCode::CREATED);

    let webhook = ctx
        .server
        .post("/monobank/webhook")
        .json(&serde_json::json!({
            "type": "StatementItem",
            "data": {
                "account": "mono-subscription-match",
                "statementItem": {
                    "id": "subscription-fx-webhook-1",
                    "time": Utc::now().timestamp(),
                    "description": "NETFLIX.COM",
                    "mcc": 4899,
                    "amount": -63960,
                    "operationAmount": -63960,
                    "currencyCode": 980,
                    "balance": 100000,
                    "hold": false
                }
            }
        }))
        .await;
    assert_eq!(webhook.status_code(), StatusCode::OK);

    for _ in 0..100 {
        let charge = ctx
            .charge_repo
            .find_by_id(charge_id, user_id)
            .await
            .expect("load charge")
            .expect("charge exists");
        if charge.match_status == ChargeMatchStatus::Matched {
            assert_eq!(charge.match_source, Some(ChargeMatchSource::Automatic));
            assert!(charge.transaction_id.is_some());
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!("webhook transaction was not matched to the pending subscription charge");
}

#[tokio::test]
async fn expense_transaction_can_create_manual_subscription() {
    let postgres = common::TestPostgres::new().await;
    let pool = postgres.pool.clone();
    let ctx = helpers::make_app_ctx(postgres.pool).await;
    let (_user_id, token) = helpers::create_test_user();
    let account_id = helpers::create_account_for(&ctx.server, &token).await;
    let category_id = helpers::create_category_for(&ctx.server, &token).await;
    let transaction_id = create_transaction(
        &ctx,
        &token,
        account_id,
        "Expense",
        Some(category_id),
        "2024-01-31T10:15:00Z",
    )
    .await;

    let response = ctx
        .server
        .post(&format!("/transactions/{transaction_id}/subscription"))
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .json(&serde_json::json!({
            "mode": "create",
            "product_name": "  Video service  ",
            "billing_period": "monthly"
        }))
        .await;

    assert_eq!(response.status_code(), StatusCode::CREATED);
    let body: Value = response.json();
    assert_eq!(body["subscription_created"], true);
    assert_eq!(body["subscription"]["provider"], "other");
    assert_eq!(body["subscription"]["product_name"], "Video service");
    assert_eq!(body["subscription"]["amount"], "19.99");
    assert_eq!(body["subscription"]["currency"], "USD");
    assert_eq!(body["subscription"]["billing_period"], "monthly");
    assert_eq!(body["subscription"]["category_id"], category_id.to_string());
    assert_eq!(body["subscription"]["started_at"], "2024-01-31T10:15:00Z");
    assert_eq!(
        body["subscription"]["next_expected_at"],
        "2024-02-29T10:15:00Z"
    );
    assert_eq!(body["charge"]["transaction_id"], transaction_id.to_string());
    assert_eq!(body["charge"]["match_status"], "Matched");
    assert_eq!(body["charge"]["kind"], "new_subscription");

    let subscription_id = body["subscription"]["id"].as_str().unwrap().to_string();
    let charge_id = body["charge"]["id"].as_str().unwrap().to_string();
    let transaction = ctx
        .server
        .get(&format!("/transactions/{transaction_id}"))
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .await;
    assert_eq!(transaction.status_code(), StatusCode::OK);
    let transaction: Value = transaction.json();
    assert_eq!(transaction["subscription_id"], subscription_id);
    assert_eq!(transaction["subscription_charge_id"], charge_id);

    let transactions = ctx
        .server
        .get("/transactions")
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .await;
    let transactions: Value = transactions.json();
    assert_eq!(transactions["items"][0]["subscription_id"], subscription_id);
    assert_eq!(
        transactions["items"][0]["subscription_charge_id"],
        charge_id
    );

    let persisted: (String, String, String) = sqlx::query_as(
        "SELECT source,match_source,s.merchant_key \
         FROM subscription_charges c JOIN subscriptions s ON s.id=c.subscription_id \
         WHERE c.id=$1",
    )
    .bind(Uuid::parse_str(&charge_id).unwrap())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(persisted.0, "manual");
    assert_eq!(persisted.1, "manual");
    assert!(persisted.2.starts_with("manual:"));

    let deleted = ctx
        .server
        .delete(&format!("/subscriptions/{subscription_id}"))
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .await;
    assert_eq!(deleted.status_code(), StatusCode::NO_CONTENT);
    let recreated = ctx
        .server
        .post(&format!("/transactions/{transaction_id}/subscription"))
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .json(&serde_json::json!({
            "mode": "create",
            "product_name": "Video service",
            "billing_period": "monthly"
        }))
        .await;
    assert_eq!(recreated.status_code(), StatusCode::CREATED);
    let recreated: Value = recreated.json();
    assert_ne!(recreated["subscription"]["id"], subscription_id);
}

#[tokio::test]
async fn transaction_can_attach_to_existing_subscription_and_retry_idempotently() {
    let postgres = common::TestPostgres::new().await;
    let ctx = helpers::make_app_ctx(postgres.pool).await;
    let (user_id, token) = helpers::create_test_user();
    let account_id = helpers::create_account_for(&ctx.server, &token).await;
    let (subscription_id, _) = seed_subscription_and_charge(&ctx, user_id).await;
    let transaction_id = create_transaction(
        &ctx,
        &token,
        account_id,
        "Expense",
        None,
        "2024-05-01T00:00:00Z",
    )
    .await;
    let request = serde_json::json!({
        "mode": "attach",
        "subscription_id": subscription_id
    });

    let first = ctx
        .server
        .post(&format!("/transactions/{transaction_id}/subscription"))
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .json(&request)
        .await;
    assert_eq!(first.status_code(), StatusCode::CREATED);
    let first_body: Value = first.json();
    assert_eq!(first_body["subscription_created"], false);
    assert_eq!(
        first_body["subscription"]["id"],
        subscription_id.to_string()
    );
    let charge_id = first_body["charge"]["id"].as_str().unwrap().to_string();

    let retry = ctx
        .server
        .post(&format!("/transactions/{transaction_id}/subscription"))
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .json(&request)
        .await;
    assert_eq!(retry.status_code(), StatusCode::OK);
    let retry_body: Value = retry.json();
    assert_eq!(retry_body["charge"]["id"], charge_id);

    let (_, other_subscription_charge) = seed_subscription_and_charge(&ctx, user_id).await;
    let other_subscription = ctx
        .charge_repo
        .find_by_id(other_subscription_charge, user_id)
        .await
        .unwrap()
        .unwrap()
        .subscription_id;
    let conflict = ctx
        .server
        .post(&format!("/transactions/{transaction_id}/subscription"))
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .json(&serde_json::json!({
            "mode": "attach",
            "subscription_id": other_subscription
        }))
        .await;
    assert_eq!(conflict.status_code(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn marking_transaction_validates_kind_input_and_ownership() {
    let postgres = common::TestPostgres::new().await;
    let ctx = helpers::make_app_ctx(postgres.pool).await;
    let (_user_id, token) = helpers::create_test_user();
    let (_other_user_id, other_token) = helpers::create_test_user();
    let account_id = helpers::create_account_for(&ctx.server, &token).await;
    let income_id = create_transaction(
        &ctx,
        &token,
        account_id,
        "Income",
        None,
        "2024-05-01T00:00:00Z",
    )
    .await;

    for payload in [
        serde_json::json!({
            "mode": "create",
            "product_name": "   ",
            "billing_period": "monthly"
        }),
        serde_json::json!({
            "mode": "create",
            "product_name": "Service",
            "billing_period": "daily"
        }),
    ] {
        let response = ctx
            .server
            .post(&format!("/transactions/{income_id}/subscription"))
            .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
            .json(&payload)
            .await;
        assert_eq!(response.status_code(), StatusCode::BAD_REQUEST);
    }

    let non_expense = ctx
        .server
        .post(&format!("/transactions/{income_id}/subscription"))
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .json(&serde_json::json!({
            "mode": "create",
            "product_name": "Salary",
            "billing_period": "monthly"
        }))
        .await;
    assert_eq!(non_expense.status_code(), StatusCode::BAD_REQUEST);

    let foreign = ctx
        .server
        .post(&format!("/transactions/{income_id}/subscription"))
        .add_header(helpers::auth(&other_token).0, helpers::auth(&other_token).1)
        .json(&serde_json::json!({
            "mode": "create",
            "product_name": "Hidden",
            "billing_period": "monthly"
        }))
        .await;
    assert_eq!(foreign.status_code(), StatusCode::NOT_FOUND);
}
