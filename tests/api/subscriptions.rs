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
    ChargeMatchStatus, ReceiptKind, SubscriptionCharge,
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
        created_at: Utc::now(),
    };
    let inserted_sub = ctx
        .subscriptions
        .subscriptions
        .upsert_by_merchant_key(&sub)
        .await
        .expect("upsert subscription");

    let charge = SubscriptionCharge {
        id: Uuid::new_v4(),
        subscription_id: inserted_sub.id,
        user_id,
        amount: dec!(15.99),
        currency: "USD".into(),
        charged_at: Utc::now(),
        email_message_id: format!("msg-{}", Uuid::new_v4()),
        kind: ReceiptKind::Renewal,
        transaction_id: None,
        match_status: ChargeMatchStatus::Pending,
        created_at: Utc::now(),
    };
    let (charge_id, _) = ctx
        .subscriptions
        .charges
        .create_idempotent(&charge)
        .await
        .expect("create charge");

    (inserted_sub.id, charge_id)
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

    let (sub_id, _) = seed_subscription_and_charge(&ctx, user_id).await;

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

    seed_subscription_and_charge(&ctx, user_id).await;

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
}

#[tokio::test]
async fn delete_subscription_returns_204_and_removes_from_list() {
    let postgres = common::TestPostgres::new().await;
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
}

#[tokio::test]
async fn subscriptions_require_auth() {
    let postgres = common::TestPostgres::new().await;
    let ctx = helpers::make_app_ctx(postgres.pool).await;

    let res = ctx.server.get("/subscriptions").await;
    assert_eq!(res.status_code(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn patch_subscription_updates_product_name() {
    let postgres = common::TestPostgres::new().await;
    let ctx = helpers::make_app_ctx(postgres.pool).await;
    let (user_id, token) = helpers::create_test_user();

    let (sub_id, _) = seed_subscription_and_charge(&ctx, user_id).await;

    let patch_res = ctx
        .server
        .patch(&format!("/subscriptions/{sub_id}"))
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .json(&serde_json::json!({ "product_name": "Netflix Standard" }))
        .await;
    assert_eq!(patch_res.status_code(), StatusCode::OK);
    let body: Value = patch_res.json();
    assert_eq!(body["product_name"], "Netflix Standard");
}
