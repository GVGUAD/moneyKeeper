use axum::http::StatusCode;
use serde_json::json;

use super::{common, helpers};

#[tokio::test]
async fn user_settings_default_is_uah() {
    let postgres = common::TestPostgres::new().await;
    let server = helpers::make_app(postgres.pool.clone()).await;
    let (_uid, token) = helpers::create_test_user();

    let res = server
        .get("/me/settings")
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .await;

    assert_eq!(res.status_code(), StatusCode::OK);
    let body: serde_json::Value = res.json();
    assert_eq!(body["base_currency"], "UAH");
}

#[tokio::test]
async fn user_settings_patch_persists() {
    let postgres = common::TestPostgres::new().await;
    helpers::seed_fx_rate(
        &postgres.pool,
        chrono::NaiveDate::from_ymd_opt(2026, 5, 10).unwrap(),
        "USD",
        rust_decimal::Decimal::new(40, 0),
    )
    .await;
    let server = helpers::make_app(postgres.pool.clone()).await;
    let (_uid, token) = helpers::create_test_user();

    let res = server
        .patch("/me/settings")
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .json(&json!({ "base_currency": "USD" }))
        .await;

    assert_eq!(res.status_code(), StatusCode::OK);
    let body: serde_json::Value = res.json();
    assert_eq!(body["base_currency"], "USD");

    let get_res = server
        .get("/me/settings")
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .await;
    assert_eq!(get_res.status_code(), StatusCode::OK);
    let body: serde_json::Value = get_res.json();
    assert_eq!(body["base_currency"], "USD");
}

#[tokio::test]
async fn user_settings_patch_rejects_unknown_currency() {
    let postgres = common::TestPostgres::new().await;
    let server = helpers::make_app(postgres.pool.clone()).await;
    let (_uid, token) = helpers::create_test_user();

    let res = server
        .patch("/me/settings")
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .json(&json!({ "base_currency": "ZZZ" }))
        .await;

    assert_eq!(res.status_code(), StatusCode::BAD_REQUEST);
}
