use axum::http::StatusCode;
use chrono::{Duration, Utc};
use uuid::Uuid;

use super::{common, helpers};
use moneykeeper::domain::email_connection::{
    EmailConnection, EmailConnectionStatus, EmailProvider,
};

#[tokio::test]
async fn oauth_start_requires_authentication() {
    let postgres = common::TestPostgres::new().await;
    let app = helpers::make_app(postgres.pool).await;
    let response = app.post("/me/email-connections/gmail/oauth/start").await;
    assert_eq!(response.status_code(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn browser_callback_is_public_but_requires_complete_parameters() {
    let postgres = common::TestPostgres::new().await;
    let app = helpers::make_app(postgres.pool).await;
    let response = app.get("/oauth/gmail/callback").await;
    assert_eq!(response.status_code(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn browser_get_callback_completes_without_auth_and_renders_safe_fallback_page() {
    let postgres = common::TestPostgres::new().await;
    let pool = postgres.pool.clone();
    let app = helpers::make_app(postgres.pool).await;
    let (user_id, token) = helpers::create_test_user();
    let start = app
        .post("/me/email-connections/gmail/oauth/start")
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .await;
    assert_eq!(start.status_code(), StatusCode::OK);
    let state = start.json::<serde_json::Value>()["state"]
        .as_str()
        .unwrap()
        .to_string();

    let callback = app
        .get(&format!(
            "/oauth/gmail/callback?code=valid-code&state={}",
            urlencoding::encode(&state)
        ))
        .await;
    assert_eq!(callback.status_code(), StatusCode::OK);
    assert_eq!(
        callback.text(),
        "Gmail connection completed. You may close this window."
    );
    assert!(!callback.text().contains("test-access-token"));
    assert!(!callback.text().contains("test-refresh-token"));

    let mailbox: (Uuid, String) =
        sqlx::query_as("SELECT id, email_address FROM email_connections WHERE user_id=$1")
            .bind(user_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(mailbox.1, "oauth-api@example.com");

    let replay = app
        .get(&format!(
            "/oauth/gmail/callback?code=valid-code&state={}",
            urlencoding::encode(&state)
        ))
        .await;
    assert_eq!(replay.status_code(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn browser_callback_uses_configured_success_and_failure_redirects() {
    let postgres = common::TestPostgres::new().await;
    let pool = postgres.pool.clone();
    let app = helpers::make_app_with_oauth_redirects(
        postgres.pool,
        Some("https://app.example.test/settings?tab=email".to_string()),
        Some("https://app.example.test/settings?tab=email".to_string()),
    )
    .await;
    let (_user_id, token) = helpers::create_test_user();

    let success_start = app
        .post("/me/email-connections/gmail/oauth/start")
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .await;
    let success_state = success_start.json::<serde_json::Value>()["state"]
        .as_str()
        .unwrap()
        .to_string();
    let success = app
        .get(&format!(
            "/oauth/gmail/callback?code=valid-code&state={}",
            urlencoding::encode(&success_state)
        ))
        .await;
    assert_eq!(success.status_code(), StatusCode::SEE_OTHER);
    let success_location = success.header("location").to_str().unwrap().to_string();
    let success_url = reqwest::Url::parse(&success_location).unwrap();
    let success_params: std::collections::HashMap<_, _> =
        success_url.query_pairs().into_owned().collect();
    assert_eq!(success_url.host_str(), Some("app.example.test"));
    assert_eq!(success_url.path(), "/settings");
    assert_eq!(success_params.get("tab").map(String::as_str), Some("email"));
    assert_eq!(
        success_params.get("status").map(String::as_str),
        Some("connected")
    );
    let connection_id = Uuid::parse_str(success_params["connection_id"].as_str()).unwrap();
    let stored_id: Uuid = sqlx::query_scalar("SELECT id FROM email_connections LIMIT 1")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(connection_id, stored_id);

    let failure_start = app
        .post("/me/email-connections/gmail/oauth/start")
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .await;
    let failure_state = failure_start.json::<serde_json::Value>()["state"]
        .as_str()
        .unwrap()
        .to_string();
    let failure = app
        .get(&format!(
            "/oauth/gmail/callback?code=rejected-code&state={}",
            urlencoding::encode(&failure_state)
        ))
        .await;
    assert_eq!(failure.status_code(), StatusCode::SEE_OTHER);
    let failure_location = failure.header("location").to_str().unwrap().to_string();
    let failure_url = reqwest::Url::parse(&failure_location).unwrap();
    let failure_params: std::collections::HashMap<_, _> =
        failure_url.query_pairs().into_owned().collect();
    assert_eq!(failure_url.host_str(), Some("app.example.test"));
    assert_eq!(
        failure_params.get("status").map(String::as_str),
        Some("error")
    );
    assert_eq!(
        failure_params.get("error").map(String::as_str),
        Some("callback_failed")
    );
}

#[tokio::test]
async fn authenticated_start_returns_pkce_authorization_state() {
    let postgres = common::TestPostgres::new().await;
    let app = helpers::make_app(postgres.pool).await;
    let (_user_id, token) = helpers::create_test_user();
    let response = app
        .post("/me/email-connections/gmail/oauth/start")
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .await;
    assert_eq!(response.status_code(), StatusCode::OK);
    let body: serde_json::Value = response.json();
    assert!(
        body["state"]
            .as_str()
            .is_some_and(|state| state.len() >= 43)
    );
    let url = body["authorize_url"].as_str().unwrap();
    assert!(url.contains("code_challenge="));
    assert!(url.contains("code_challenge_method=S256"));
    assert!(!url.contains("test-client-secret"));
}

#[tokio::test]
async fn provider_denial_requires_and_consumes_one_time_state() {
    let postgres = common::TestPostgres::new().await;
    let pool = postgres.pool.clone();
    let app = helpers::make_app(postgres.pool).await;
    let (_user_id, token) = helpers::create_test_user();
    let start = app
        .post("/me/email-connections/gmail/oauth/start")
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .await;
    let state = start.json::<serde_json::Value>()["state"]
        .as_str()
        .unwrap()
        .to_string();

    let denial = app
        .get(&format!(
            "/oauth/gmail/callback?error=access_denied&state={}",
            urlencoding::encode(&state)
        ))
        .await;
    assert_eq!(denial.status_code(), StatusCode::BAD_REQUEST);
    let consumed: bool = sqlx::query_scalar(
        "SELECT consumed_at IS NOT NULL FROM gmail_oauth_states ORDER BY created_at DESC LIMIT 1",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(consumed);

    let replay = app
        .get(&format!(
            "/oauth/gmail/callback?error=access_denied&state={}",
            urlencoding::encode(&state)
        ))
        .await;
    assert_eq!(replay.status_code(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn compatibility_callback_rejects_unknown_state() {
    let postgres = common::TestPostgres::new().await;
    let app = helpers::make_app(postgres.pool).await;
    let (_user_id, token) = helpers::create_test_user();
    let response = app
        .post("/me/email-connections/gmail/oauth/callback")
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .json(&serde_json::json!({ "code": "code", "state": "unknown" }))
        .await;
    assert_eq!(response.status_code(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn deprecated_post_callback_completes_and_rejects_state_replay() {
    let postgres = common::TestPostgres::new().await;
    let app = helpers::make_app(postgres.pool).await;
    let (_user_id, token) = helpers::create_test_user();
    let start = app
        .post("/me/email-connections/gmail/oauth/start")
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .await;
    let state = start.json::<serde_json::Value>()["state"]
        .as_str()
        .unwrap()
        .to_string();

    let complete = app
        .post("/me/email-connections/gmail/oauth/callback")
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .json(&serde_json::json!({ "code": "valid-code", "state": state }))
        .await;
    assert_eq!(complete.status_code(), StatusCode::CREATED);
    let body: serde_json::Value = complete.json();
    assert_eq!(body["email_address"], "oauth-api@example.com");
    assert!(body.get("oauth_access_token").is_none());
    assert!(body.get("oauth_refresh_token").is_none());

    let replay = app
        .post("/me/email-connections/gmail/oauth/callback")
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .json(&serde_json::json!({ "code": "valid-code", "state": state }))
        .await;
    assert_eq!(replay.status_code(), StatusCode::BAD_REQUEST);
}

fn connected_mailbox(user_id: Uuid, address: &str) -> EmailConnection {
    EmailConnection {
        id: Uuid::new_v4(),
        user_id,
        provider: EmailProvider::Gmail,
        email_address: address.to_string(),
        oauth_access_token: "test-access".into(),
        oauth_refresh_token: "test-refresh".into(),
        credential_version: 0,
        access_token_expires_at: Utc::now() + Duration::hours(1),
        status: EmailConnectionStatus::Connected,
        last_synced_at: None,
        last_history_id: None,
        created_at: Utc::now(),
    }
}

#[tokio::test]
async fn manual_resync_returns_202_and_active_lease_returns_409() {
    let postgres = common::TestPostgres::new().await;
    let pool = postgres.pool.clone();
    let ctx = helpers::make_app_ctx(postgres.pool).await;
    let (user_id, token) = helpers::create_test_user();

    let accepted = connected_mailbox(user_id, "accepted@example.com");
    ctx.email_connection_repo
        .create(&accepted)
        .await
        .expect("create accepted mailbox");
    let response = ctx
        .server
        .post(&format!("/me/email-connections/{}/resync", accepted.id))
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .await;
    assert_eq!(response.status_code(), StatusCode::ACCEPTED);

    let busy = connected_mailbox(user_id, "busy@example.com");
    ctx.email_connection_repo
        .create(&busy)
        .await
        .expect("create busy mailbox");
    sqlx::query(
        "UPDATE email_connections SET sync_lease_owner=$1, sync_lease_expires_at=$2 WHERE id=$3",
    )
    .bind(Uuid::new_v4())
    .bind((Utc::now() + Duration::minutes(10)).timestamp())
    .bind(busy.id)
    .execute(&pool)
    .await
    .expect("seed active lease");

    let response = ctx
        .server
        .post(&format!("/me/email-connections/{}/resync", busy.id))
        .add_header(helpers::auth(&token).0, helpers::auth(&token).1)
        .await;
    assert_eq!(response.status_code(), StatusCode::CONFLICT);
}
