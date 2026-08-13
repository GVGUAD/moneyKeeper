use std::sync::Arc;

use axum::http::{StatusCode, header::AUTHORIZATION};
use axum_test::TestServer;
use chrono::{TimeZone, Utc};
use moneykeeper::contexts::ledger::public::{
    AccountKind, AccountNature, ObservationId, ObserveProviderBalance, OpenAccount, SourceReference,
};
use moneykeeper::shared_kernel::{CorrelationId, CurrencyCode, IdempotencyKey, Money, UserId};
use rust_decimal::Decimal;
use serde_json::{Value, json};
use uuid::Uuid;

#[path = "v2_test_support.rs"]
mod v2_test_support;

const TEST_KID: &str = "test-key-1";
const TEST_EC_PRIVATE_KEY: &str = "-----BEGIN PRIVATE KEY-----
MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgN/zCeuQq48O/tp5y
b50qbJqKns6bCt6JctzISKw2sPqhRANCAASdCO4KOpDloBoTURgT/ZeiWey7OSIG
46TJaP8IugkOaxHZ6HCuZvK4AaDOXLZHOyRHEWK5AhPl1f98M4xYBkQy
-----END PRIVATE KEY-----";

fn test_jwks() -> jsonwebtoken::jwk::JwkSet {
    serde_json::from_value(json!({"keys": [{
        "kty": "EC", "crv": "P-256", "kid": TEST_KID, "alg": "ES256", "use": "sig",
        "x": "nQjuCjqQ5aAaE1EYE_2XolnsuzkiBuOkyWj_CLoJDms",
        "y": "EdnocK5m8rgBoM5ctkc7JEcRYrkCE-XV_3wzjFgGRDI"
    }]}))
    .unwrap()
}

fn jwt(user_id: Uuid) -> String {
    use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
    #[derive(serde::Serialize)]
    struct Claims {
        sub: String,
        aud: String,
        role: String,
        exp: i64,
        iat: i64,
    }
    let now = chrono::Utc::now().timestamp();
    let mut header = Header::new(Algorithm::ES256);
    header.kid = Some(TEST_KID.to_owned());
    encode(
        &header,
        &Claims {
            sub: user_id.to_string(),
            aud: "authenticated".to_owned(),
            role: "authenticated".to_owned(),
            exp: now + 3_600,
            iat: now,
        },
        &EncodingKey::from_ec_pem(TEST_EC_PRIVATE_KEY.as_bytes()).unwrap(),
    )
    .unwrap()
}

async fn app(user_id: Uuid) -> TestServer {
    let database = v2_test_support::fresh_v2_database().await;
    let verified = database.initialize().await.unwrap();
    let mut server = TestServer::new(moneykeeper::bootstrap::v2::router(
        &verified,
        Arc::new(test_jwks()),
    ))
    .unwrap();
    server.add_header(AUTHORIZATION, format!("Bearer {}", jwt(user_id)));
    server
}

fn account_body(amount: &str, occurred_at: &str) -> Value {
    json!({
        "name": "Wallet", "currency": "UAH", "kind": "cash", "nature": "asset",
        "opening_balance": amount, "occurred_at": occurred_at
    })
}

#[tokio::test]
async fn money_and_idempotency_are_validated_before_ledger_execution() {
    let server = app(Uuid::new_v4()).await;
    let at = "2026-08-13T09:00:00Z";

    let missing = server
        .post("/accounts")
        .json(&account_body("1.00", at))
        .await;
    assert_eq!(missing.status_code(), StatusCode::BAD_REQUEST);
    assert_eq!(
        missing.json::<Value>()["error"],
        "missing Idempotency-Key header"
    );

    let oversized = server
        .post("/accounts")
        .add_header("Idempotency-Key", "x".repeat(201))
        .json(&account_body("1.00", at))
        .await;
    assert_eq!(oversized.status_code(), StatusCode::BAD_REQUEST);

    let excess_scale = server
        .post("/accounts")
        .add_header("Idempotency-Key", "scale")
        .json(&account_body("1.001", at))
        .await;
    assert_eq!(excess_scale.status_code(), StatusCode::BAD_REQUEST);

    let unknown = server.post("/accounts").add_header("Idempotency-Key", "unknown")
        .json(&json!({"name":"X","currency":"GBP","kind":"cash","nature":"asset","opening_balance":"1","occurred_at":at})).await;
    assert_eq!(unknown.status_code(), StatusCode::BAD_REQUEST);

    let first = server
        .post("/accounts")
        .add_header("Idempotency-Key", "canonical")
        .json(&account_body("1.00", at))
        .await;
    assert_eq!(first.status_code(), StatusCode::CREATED);
    assert_eq!(first.json::<Value>()["replayed"], false);
    let replay = server
        .post("/accounts")
        .add_header("Idempotency-Key", "canonical")
        .json(&account_body("1", at))
        .await;
    assert_eq!(replay.status_code(), StatusCode::CREATED);
    assert_eq!(replay.json::<Value>()["replayed"], true);

    let conflict = server
        .post("/accounts")
        .add_header("Idempotency-Key", "canonical")
        .json(&account_body("2", at))
        .await;
    assert_eq!(conflict.status_code(), StatusCode::CONFLICT);
    assert_eq!(conflict.json::<Value>()["error"], "ledger conflict");
}

#[tokio::test]
async fn account_transaction_annotation_and_correction_routes_preserve_history() {
    let server = app(Uuid::new_v4()).await;
    let at = "2026-08-13T10:00:00Z";
    let opened = server
        .post("/accounts")
        .add_header("Idempotency-Key", "open-wallet")
        .json(&account_body("100.00", at))
        .await;
    assert_eq!(opened.status_code(), StatusCode::CREATED);
    let account_id = opened.json::<Value>()["account"]["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let transaction = server
        .post("/transactions")
        .add_header("Idempotency-Key", "expense-1")
        .json(&json!({
            "account_id": account_id, "kind":"expense",
            "amount":{"amount":"12.50","currency":"UAH"}, "description":"Lunch",
            "tags":[" Food ","food"], "budget_visibility":"included", "occurred_at":at
        }))
        .await;
    assert_eq!(transaction.status_code(), StatusCode::CREATED);
    let journal_id = transaction.json::<Value>()["journal_entry_id"]
        .as_str()
        .unwrap()
        .to_owned();

    let detail = server.get(&format!("/transactions/{journal_id}")).await;
    assert_eq!(detail.status_code(), StatusCode::OK);
    assert_eq!(
        detail.json::<Value>()["postings"].as_array().unwrap().len(),
        2
    );
    let activity = server
        .get(&format!("/accounts/{account_id}/activity?limit=10"))
        .await;
    assert_eq!(activity.status_code(), StatusCode::OK);
    assert_eq!(activity.json::<Value>().as_array().unwrap().len(), 2);

    let missing_version = server
        .patch(&format!("/transactions/{journal_id}/annotation"))
        .add_header("Idempotency-Key", "annotate-missing")
        .json(&json!({"description":"Dinner"}))
        .await;
    assert_eq!(missing_version.status_code(), StatusCode::BAD_REQUEST);
    let annotated = server
        .patch(&format!("/transactions/{journal_id}/annotation"))
        .add_header("Idempotency-Key", "annotate-1")
        .json(&json!({"description":"Dinner","expected_version":1,"occurred_at":at}))
        .await;
    assert_eq!(annotated.status_code(), StatusCode::OK);
    assert_eq!(annotated.json::<Value>()["version"], 2);

    let stale = server
        .post(&format!("/accounts/{account_id}/balance-corrections"))
        .add_header("Idempotency-Key", "stale-correction")
        .json(&json!({
            "target_display_balance":{"amount":"90","currency":"UAH"},
            "expected_balance_version":1,"reason":"Count","observed_at":at,"occurred_at":at
        }))
        .await;
    assert_eq!(stale.status_code(), StatusCode::CONFLICT);
    assert_eq!(
        server
            .delete(&format!("/transactions/{journal_id}"))
            .await
            .status_code(),
        StatusCode::METHOD_NOT_ALLOWED
    );
}

#[tokio::test]
async fn ledger_queries_hide_other_tenants() {
    let database = v2_test_support::fresh_v2_database().await;
    let verified = database.initialize().await.unwrap();
    let router = moneykeeper::bootstrap::v2::router(&verified, Arc::new(test_jwks()));
    let owner = Uuid::new_v4();
    let mut owner_server = TestServer::new(router.clone()).unwrap();
    owner_server.add_header(AUTHORIZATION, format!("Bearer {}", jwt(owner)));
    let opened = owner_server
        .post("/accounts")
        .add_header("Idempotency-Key", "tenant-open")
        .json(&account_body("0", "2026-08-13T11:00:00Z"))
        .await;
    let id = opened.json::<Value>()["account"]["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let mut stranger = TestServer::new(router).unwrap();
    stranger.add_header(AUTHORIZATION, format!("Bearer {}", jwt(Uuid::new_v4())));
    assert_eq!(
        stranger.get(&format!("/accounts/{id}")).await.status_code(),
        StatusCode::NOT_FOUND
    );
    assert!(
        stranger
            .get("/accounts")
            .await
            .json::<Value>()
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn reconciliation_routes_require_versions_and_expose_only_tenant_cases() {
    let database = v2_test_support::fresh_v2_database().await;
    let verified = database.initialize().await.unwrap();
    let contexts = moneykeeper::bootstrap::v2::supporting_contexts(&verified);
    let user_uuid = Uuid::new_v4();
    let user = UserId::new(user_uuid);
    let currency = CurrencyCode::new("UAH").unwrap();
    let account = contexts
        .ledger
        .open_account(OpenAccount {
            user_id: user,
            name: "API bank".to_owned(),
            currency: currency.clone(),
            kind: AccountKind::Cash,
            nature: AccountNature::Asset,
            opening_balance: Money::new(Decimal::new(1000, 2), currency.clone(), 2).unwrap(),
            idempotency_key: IdempotencyKey::new("api-reconcile-open").unwrap(),
            correlation_id: CorrelationId::generate(),
            causation_id: None,
            occurred_at: Utc.with_ymd_and_hms(2026, 8, 13, 13, 0, 0).unwrap(),
        })
        .await
        .unwrap();
    let observed_at = Utc.with_ymd_and_hms(2026, 8, 13, 13, 1, 0).unwrap();
    let pending = contexts
        .ledger
        .observe_provider_balance(ObserveProviderBalance {
            user_id: user,
            account_id: account.account.id,
            observation_id: ObservationId::generate(),
            source: SourceReference::new("banking", "api-stream", "balance-1").unwrap(),
            provider_reported: Money::new(Decimal::new(1200, 2), currency, 2).unwrap(),
            available: None,
            observed_at,
            source_sequence: 1,
            idempotency_key: IdempotencyKey::new("api-observe").unwrap(),
            correlation_id: CorrelationId::generate(),
            causation_id: None,
        })
        .await
        .unwrap();
    let router = moneykeeper::bootstrap::v2::router(&verified, Arc::new(test_jwks()));
    let mut server = TestServer::new(router.clone()).unwrap();
    server.add_header(AUTHORIZATION, format!("Bearer {}", jwt(user_uuid)));

    let listed = server.get("/reconciliations").await;
    assert_eq!(listed.status_code(), StatusCode::OK);
    assert_eq!(listed.json::<Value>().as_array().unwrap().len(), 1);
    let case_id = pending.case.id.to_string();
    assert_eq!(
        server
            .get(&format!("/reconciliations/{case_id}"))
            .await
            .status_code(),
        StatusCode::OK
    );
    let missing = server
        .post(&format!("/reconciliations/{case_id}/approve"))
        .add_header("Idempotency-Key", "api-approve-missing")
        .json(&json!({"reason":"Statement"}))
        .await;
    assert_eq!(missing.status_code(), StatusCode::BAD_REQUEST);
    let stale = server.post(&format!("/reconciliations/{case_id}/approve"))
        .add_header("Idempotency-Key", "api-approve-stale")
        .json(&json!({"expected_version":1,"expected_balance_version":999,"reason":"Statement","occurred_at":observed_at})).await;
    assert_eq!(stale.status_code(), StatusCode::CONFLICT);
    let approved = server.post(&format!("/reconciliations/{case_id}/approve"))
        .add_header("Idempotency-Key", "api-approve")
        .json(&json!({"expected_version":1,"expected_balance_version":pending.case.captured_balance_version.get(),"reason":"Statement","occurred_at":observed_at})).await;
    assert_eq!(approved.status_code(), StatusCode::OK);
    assert_eq!(approved.json::<Value>()["case"]["status"], "approved");

    let mut stranger = TestServer::new(router).unwrap();
    stranger.add_header(AUTHORIZATION, format!("Bearer {}", jwt(Uuid::new_v4())));
    assert_eq!(
        stranger
            .get(&format!("/reconciliations/{case_id}"))
            .await
            .status_code(),
        StatusCode::NOT_FOUND
    );
}
