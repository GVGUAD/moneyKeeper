use axum::http::StatusCode;
use serde_json::Value;
use uuid::Uuid;

use super::{common, helpers};

#[tokio::test]
async fn create_category_returns_201() {
    let postgres = common::TestPostgres::new().await;
    let server = helpers::make_app(postgres.pool).await;
    let (_uid, token) = helpers::create_test_user();

    let res = server
        .post("/categories")
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .json(&serde_json::json!({ "name": "Food" }))
        .await;

    assert_eq!(res.status_code(), StatusCode::CREATED);
    let body: Value = res.json();
    assert!(body["id"].is_string());
    assert_eq!(body["name"], "Food");
    assert!(body["color"].is_null());
}

#[tokio::test]
async fn create_category_with_color_returns_201() {
    let postgres = common::TestPostgres::new().await;
    let server = helpers::make_app(postgres.pool).await;
    let (_uid, token) = helpers::create_test_user();

    let res = server
        .post("/categories")
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .json(&serde_json::json!({ "name": "Transport", "color": "#ff0000" }))
        .await;

    assert_eq!(res.status_code(), StatusCode::CREATED);
    let body: Value = res.json();
    assert_eq!(body["color"], "#ff0000");
}

#[tokio::test]
async fn create_category_no_auth_returns_401() {
    let postgres = common::TestPostgres::new().await;
    let server = helpers::make_app(postgres.pool).await;

    let res = server
        .post("/categories")
        .json(&serde_json::json!({ "name": "Misc" }))
        .await;

    assert_eq!(res.status_code(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn list_categories_empty_returns_empty_array() {
    let postgres = common::TestPostgres::new().await;
    let server = helpers::make_app(postgres.pool).await;
    let (_uid, token) = helpers::create_test_user();

    let res = server
        .get("/categories")
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .await;

    assert_eq!(res.status_code(), StatusCode::OK);
    let body: Value = res.json();
    assert_eq!(body, serde_json::json!([]));
}

#[tokio::test]
async fn list_categories_returns_own_only() {
    let postgres = common::TestPostgres::new().await;
    let server = helpers::make_app(postgres.pool).await;
    let (_uid1, token1) = helpers::create_test_user();
    let (_uid2, token2) = helpers::create_test_user();

    helpers::create_category_for(&server, &token1).await;
    helpers::create_category_for(&server, &token1).await;
    helpers::create_category_for(&server, &token2).await;

    let res1 = server
        .get("/categories")
        .add_header(helpers::auth(&token1).0, helpers::auth(&token1).1)
        .await;
    assert_eq!(res1.json::<Value>().as_array().unwrap().len(), 2);

    let res2 = server
        .get("/categories")
        .add_header(helpers::auth(&token2).0, helpers::auth(&token2).1)
        .await;
    assert_eq!(res2.json::<Value>().as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn update_category_name_returns_200() {
    let postgres = common::TestPostgres::new().await;
    let server = helpers::make_app(postgres.pool).await;
    let (_uid, token) = helpers::create_test_user();
    let cat_id = helpers::create_category_for(&server, &token).await;

    let res = server
        .put(&format!("/categories/{cat_id}"))
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .json(&serde_json::json!({ "name": "New Name" }))
        .await;

    assert_eq!(res.status_code(), StatusCode::OK);
    let body: Value = res.json();
    assert_eq!(body["name"], "New Name");
}

#[tokio::test]
async fn update_category_color_to_null_returns_200() {
    let postgres = common::TestPostgres::new().await;
    let server = helpers::make_app(postgres.pool).await;
    let (_uid, token) = helpers::create_test_user();

    // Create with color
    let res = server
        .post("/categories")
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .json(&serde_json::json!({ "name": "Colored", "color": "#aabbcc" }))
        .await;
    let cat_id: Uuid = Uuid::parse_str(res.json::<Value>()["id"].as_str().unwrap()).unwrap();

    // Clear the color
    let res = server
        .put(&format!("/categories/{cat_id}"))
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .json(&serde_json::json!({ "color": null }))
        .await;

    assert_eq!(res.status_code(), StatusCode::OK);
    let body: Value = res.json();
    assert!(body["color"].is_null());
}

#[tokio::test]
async fn update_category_not_found_returns_404() {
    let postgres = common::TestPostgres::new().await;
    let server = helpers::make_app(postgres.pool).await;
    let (_uid, token) = helpers::create_test_user();
    let random_id = Uuid::new_v4();

    let res = server
        .put(&format!("/categories/{random_id}"))
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .json(&serde_json::json!({ "name": "X" }))
        .await;

    assert_eq!(res.status_code(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn delete_category_returns_204() {
    let postgres = common::TestPostgres::new().await;
    let server = helpers::make_app(postgres.pool).await;
    let (_uid, token) = helpers::create_test_user();
    let cat_id = helpers::create_category_for(&server, &token).await;

    let res = server
        .delete(&format!("/categories/{cat_id}"))
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .await;

    assert_eq!(res.status_code(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn delete_category_not_found_returns_404() {
    let postgres = common::TestPostgres::new().await;
    let server = helpers::make_app(postgres.pool).await;
    let (_uid, token) = helpers::create_test_user();
    let random_id = Uuid::new_v4();

    let res = server
        .delete(&format!("/categories/{random_id}"))
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .await;

    assert_eq!(res.status_code(), StatusCode::NOT_FOUND);
}
