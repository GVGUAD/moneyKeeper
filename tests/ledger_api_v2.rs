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
    let detail: Value = detail.json();
    assert_eq!(detail["postings"].as_array().unwrap().len(), 2);
    assert_eq!(detail["source"], "manual");
    assert_eq!(detail["actor"]["kind"], "user");
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
    let approved: Value = approved.json();
    assert_eq!(approved["case"]["status"], "approved");
    let correction_id = approved["journal_entry_id"].as_str().unwrap();
    let correction: Value = server
        .get(&format!("/transactions/{correction_id}"))
        .await
        .json();
    assert_eq!(correction["source"], "reconciliation");
    assert_eq!(correction["correction"]["before_display_balance"], "10.00");
    assert_eq!(correction["correction"]["target_display_balance"], "12.00");
    assert_eq!(correction["correction"]["display_delta"], "2.00");

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

#[tokio::test]
async fn complete_visible_money_lifecycle_and_tamper_recovery() {
    let (verified, pool) = v2_test_support::fresh_v2_runtime().await;
    let contexts = moneykeeper::bootstrap::v2::supporting_contexts(&verified);
    let user_uuid = Uuid::new_v4();
    let user = UserId::new(user_uuid);
    let mut server = TestServer::new(moneykeeper::bootstrap::v2::router(
        &verified,
        Arc::new(test_jwks()),
    ))
    .unwrap();
    server.add_header(AUTHORIZATION, format!("Bearer {}", jwt(user_uuid)));
    let at = Utc.with_ymd_and_hms(2026, 8, 13, 16, 0, 0).unwrap();
    let open = |name: &str, amount: &str| json!({"name":name,"currency":"UAH","kind":"cash","nature":"asset","opening_balance":amount,"occurred_at":at});
    let cash: Value = server
        .post("/accounts")
        .add_header("Idempotency-Key", "life-cash")
        .json(&open("Cash", "100.00"))
        .await
        .json();
    let card: Value = server
        .post("/accounts")
        .add_header("Idempotency-Key", "life-card")
        .json(&open("Debit card", "0"))
        .await
        .json();
    let cash_id = cash["account"]["id"].as_str().unwrap();
    let card_id = card["account"]["id"].as_str().unwrap();
    let expense = |key: &str, account_id: &str, amount: &str| json!({"account_id":account_id,"kind":"expense","amount":{"amount":amount,"currency":"UAH"},"description":key,"occurred_at":at});
    let first: Value = server
        .post("/transactions")
        .add_header("Idempotency-Key", "life-expense-1")
        .json(&expense("First", cash_id, "10"))
        .await
        .json();
    let second: Value = server
        .post("/transactions")
        .add_header("Idempotency-Key", "life-expense-2")
        .json(&expense("Second", cash_id, "5"))
        .await
        .json();
    let first_id = first["journal_entry_id"].as_str().unwrap();
    let second_id = second["journal_entry_id"].as_str().unwrap();
    assert_eq!(server.post("/transfers").add_header("Idempotency-Key", "life-transfer").json(&json!({
        "source_account_id":cash_id,"target_account_id":card_id,
        "source_amount":{"amount":"20","currency":"UAH"},"target_amount":{"amount":"20","currency":"UAH"},
        "fee":{"amount":"2","currency":"UAH"},"description":"Fund card","occurred_at":at
    })).await.status_code(), StatusCode::CREATED);
    let card_before: Value = server.get(&format!("/accounts/{card_id}")).await.json();
    let correction: Value = server
        .post(&format!("/accounts/{card_id}/balance-corrections"))
        .add_header("Idempotency-Key", "life-correct")
        .json(&json!({
            "target_display_balance":{"amount":"25","currency":"UAH"},
            "expected_balance_version":card_before["balance_version"],"reason":"Counted card",
            "observed_at":at,"occurred_at":at
        }))
        .await
        .json();
    assert_eq!(correction["effects"][0]["display_balance"], "25");
    assert_eq!(
        server
            .post(&format!("/transactions/{first_id}/reversals"))
            .add_header("Idempotency-Key", "life-reverse")
            .json(&json!({"reason":"Duplicate","occurred_at":at}))
            .await
            .status_code(),
        StatusCode::CREATED
    );
    let replacement: Value = server
        .post(&format!("/transactions/{second_id}/replacements"))
        .add_header("Idempotency-Key", "life-replace")
        .json(&json!({
            "account_id":cash_id,"kind":"expense","amount":{"amount":"7","currency":"UAH"},
            "description":"Corrected second","occurred_at":at
        }))
        .await
        .json();
    let replacement_id = replacement["replacement_journal_entry_id"]
        .as_str()
        .unwrap();
    assert_eq!(
        server
            .patch(&format!("/transactions/{replacement_id}/annotation"))
            .add_header("Idempotency-Key", "life-annotation")
            .json(
                &json!({"expected_version":1,"note":"reviewed","tags":["final"],"occurred_at":at})
            )
            .await
            .status_code(),
        StatusCode::OK
    );
    assert_eq!(
        server
            .post(&format!("/accounts/{card_id}/archive"))
            .add_header("Idempotency-Key", "life-archive")
            .json(&json!({"expected_version":1,"occurred_at":at}))
            .await
            .status_code(),
        StatusCode::OK
    );
    assert_eq!(
        server
            .post("/transactions")
            .add_header("Idempotency-Key", "life-blocked")
            .json(&expense("Blocked", card_id, "1"))
            .await
            .status_code(),
        StatusCode::CONFLICT
    );
    let archived: Value = server.get(&format!("/accounts/{card_id}")).await.json();
    assert_eq!(archived["lifecycle"], "archived");
    assert_eq!(archived["display_balance"], "25");
    assert_eq!(
        server
            .post(&format!("/accounts/{card_id}/restore"))
            .add_header("Idempotency-Key", "life-restore")
            .json(&json!({"expected_version":2,"occurred_at":at}))
            .await
            .status_code(),
        StatusCode::OK
    );

    let card_account = moneykeeper::contexts::ledger::public::LedgerAccountId::new(
        Uuid::parse_str(card_id).unwrap(),
    );
    let observe = |item: &str, amount: i64, sequence: i64, key: &str| ObserveProviderBalance {
        user_id: user,
        account_id: card_account,
        observation_id: ObservationId::generate(),
        source: SourceReference::new("banking", "lifecycle-card", item).unwrap(),
        provider_reported: Money::new(
            Decimal::new(amount, 2),
            CurrencyCode::new("UAH").unwrap(),
            2,
        )
        .unwrap(),
        available: None,
        observed_at: at + chrono::Duration::seconds(sequence),
        source_sequence: sequence,
        idempotency_key: IdempotencyKey::new(key).unwrap(),
        correlation_id: CorrelationId::generate(),
        causation_id: None,
    };
    let matched = contexts
        .ledger
        .observe_provider_balance(observe("matched", 2500, 1, "life-observe-matched"))
        .await
        .unwrap();
    assert_eq!(
        matched.case.status,
        moneykeeper::contexts::ledger::public::ReconciliationStatus::Matched
    );
    assert!(matched.journal_entry_id.is_none());
    let pending = contexts
        .ledger
        .observe_provider_balance(observe("pending", 3000, 2, "life-observe-pending"))
        .await
        .unwrap();
    assert_eq!(server.post(&format!("/reconciliations/{}/approve", pending.case.id))
        .add_header("Idempotency-Key", "life-approve").json(&json!({
            "expected_version":1,"expected_balance_version":pending.case.captured_balance_version.get(),
            "reason":"Provider statement","occurred_at":at
        })).await.status_code(), StatusCode::OK);
    let stale = contexts
        .ledger
        .observe_provider_balance(observe("stale", 3500, 3, "life-observe-stale"))
        .await
        .unwrap();
    assert_eq!(
        server
            .post("/transactions")
            .add_header("Idempotency-Key", "life-intervening")
            .json(&expense("Intervening", card_id, "1"))
            .await
            .status_code(),
        StatusCode::CREATED
    );
    assert_eq!(server.post(&format!("/reconciliations/{}/approve", stale.case.id))
        .add_header("Idempotency-Key", "life-stale-approve").json(&json!({
            "expected_version":1,"expected_balance_version":stale.case.captured_balance_version.get(),
            "reason":"Too late","occurred_at":at
        })).await.status_code(), StatusCode::CONFLICT);
    let activity: Value = server
        .get(&format!("/accounts/{card_id}/activity?limit=50"))
        .await
        .json();
    assert!(
        activity
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["source"] == "reconciliation"
                && entry["correction"]["display_delta"] == "5.00")
    );
    assert!(
        activity
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["source"] == "manual")
    );

    sqlx::query("UPDATE ledger.account_balances SET signed_balance = signed_balance + 1 WHERE account_id = $1 AND user_id = $2")
        .bind(card_account.into_uuid()).bind(user.into_uuid()).execute(&pool).await.unwrap();
    assert_eq!(contexts.ledger.verify_projection().await.unwrap().len(), 1);
    contexts.ledger.rebuild_projection().await.unwrap();
    assert!(
        contexts
            .ledger
            .verify_projection()
            .await
            .unwrap()
            .is_empty()
    );
    let journal_id = Uuid::parse_str(first_id).unwrap();
    assert!(
        sqlx::query("UPDATE ledger.journal_entries SET description = 'tampered' WHERE id = $1")
            .bind(journal_id)
            .execute(&pool)
            .await
            .is_err()
    );
    assert!(
        sqlx::query("DELETE FROM ledger.postings WHERE journal_entry_id = $1")
            .bind(journal_id)
            .execute(&pool)
            .await
            .is_err()
    );
}
