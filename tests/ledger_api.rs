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

#[path = "test_support.rs"]
mod test_support;

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
    app_with_contexts(user_id).await.0
}

async fn app_with_contexts(user_id: Uuid) -> (TestServer, moneykeeper::bootstrap::ContextFacades) {
    let database = test_support::fresh_database().await;
    let verified = database.initialize().await.unwrap();
    let mut server = TestServer::new(moneykeeper::bootstrap::router(
        &verified,
        Arc::new(test_jwks()),
    ))
    .unwrap();
    server.add_header(AUTHORIZATION, format!("Bearer {}", jwt(user_id)));
    (server, moneykeeper::bootstrap::build_contexts(&verified))
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
        .json(&json!({"name":"X","currency":"RUB","kind":"cash","nature":"asset","opening_balance":"1","occurred_at":at})).await;
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
async fn transaction_activity_range_filter_and_summary_are_additive_and_validated() {
    let server = app(Uuid::new_v4()).await;
    let opened = server
        .post("/accounts")
        .add_header("Idempotency-Key", "activity-api-open")
        .json(&account_body("0", "2026-08-12T09:00:00Z"))
        .await;
    assert_eq!(opened.status_code(), StatusCode::CREATED);
    let account_id = opened.json::<Value>()["account"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    for (key, kind, amount, occurred_at) in [
        (
            "activity-api-expense",
            "expense",
            "15.00",
            "2026-08-13T10:00:00Z",
        ),
        (
            "activity-api-income",
            "income",
            "20.00",
            "2026-08-13T11:00:00Z",
        ),
        (
            "activity-api-boundary",
            "expense",
            "99.00",
            "2026-08-14T00:00:00Z",
        ),
    ] {
        let response = server
            .post("/transactions")
            .add_header("Idempotency-Key", key)
            .json(&json!({
                "account_id": account_id,
                "kind": kind,
                "amount": {"amount": amount, "currency": "UAH"},
                "description": key,
                "occurred_at": occurred_at
            }))
            .await;
        assert_eq!(response.status_code(), StatusCode::CREATED);
    }

    let legacy = server.get("/transactions?limit=50").await;
    assert_eq!(legacy.status_code(), StatusCode::OK);
    assert_eq!(legacy.json::<Vec<Value>>().len(), 3);

    let range = "from_occurred_at=2026-08-13T00:00:00Z&before_occurred_at=2026-08-14T00:00:00Z";
    let page = server
        .get(&format!("/transactions?{range}&kind=all&limit=1"))
        .await;
    assert_eq!(page.status_code(), StatusCode::OK);
    let page = page.json::<Vec<Value>>();
    assert_eq!(page.len(), 1);
    assert_eq!(page[0]["annotation"]["description"], "activity-api-income");
    let next = server
        .get(&format!(
            "/transactions?{range}&kind=all&limit=1&after_occurred_at={}&after_sequence={}",
            page[0]["occurred_at"].as_str().unwrap(),
            page[0]["ledger_sequence"].as_i64().unwrap()
        ))
        .await;
    assert_eq!(next.status_code(), StatusCode::OK);
    let next = next.json::<Vec<Value>>();
    assert_eq!(next.len(), 1);
    assert_eq!(next[0]["annotation"]["description"], "activity-api-expense");

    let summary = server
        .get(&format!("/transactions/summary?{range}&kind=all"))
        .await;
    assert_eq!(summary.status_code(), StatusCode::OK);
    let summary = summary.json::<Value>();
    assert_eq!(summary["transaction_count"], 2);
    assert_eq!(summary["category_count"], 0);
    assert_eq!(summary["totals"][0]["currency"], "UAH");
    assert_eq!(
        summary["totals"][0]["amount"]
            .as_str()
            .unwrap()
            .parse::<Decimal>()
            .unwrap(),
        Decimal::new(500, 2)
    );
    let expense = server
        .get(&format!("/transactions/summary?{range}&kind=expense"))
        .await
        .json::<Value>();
    assert_eq!(expense["transaction_count"], 1);
    assert_eq!(
        expense["totals"][0]["amount"]
            .as_str()
            .unwrap()
            .parse::<Decimal>()
            .unwrap(),
        Decimal::new(-1500, 2)
    );

    for invalid in [
        "/transactions?from_occurred_at=2026-08-13T00:00:00Z",
        "/transactions?kind=income",
        "/transactions?from_occurred_at=2026-08-14T00:00:00Z&before_occurred_at=2026-08-13T00:00:00Z",
        "/transactions/summary?before_occurred_at=2026-08-14T00:00:00Z",
    ] {
        assert_eq!(
            server.get(invalid).await.status_code(),
            StatusCode::BAD_REQUEST
        );
    }
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
    assert_eq!(detail["annotation"]["version"], 1);
    assert_eq!(detail["annotation"]["description"], "Lunch");
    assert_eq!(detail["annotation"]["note"], Value::Null);
    assert_eq!(detail["annotation"]["tags"], json!(["food"]));
    assert_eq!(detail["annotation"]["budget_visibility"], "included");
    assert_eq!(detail["reversed_by_journal_id"], Value::Null);
    assert_eq!(detail["replaced_by_journal_id"], Value::Null);
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
    let annotated_detail: Value = server
        .get(&format!("/transactions/{journal_id}"))
        .await
        .json();
    assert_eq!(annotated_detail["description"], "Lunch");
    assert_eq!(annotated_detail["annotation"]["description"], "Dinner");
    assert_eq!(annotated_detail["annotation"]["version"], 2);

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
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn ledger_queries_hide_other_tenants() {
    let database = test_support::fresh_database().await;
    let verified = database.initialize().await.unwrap();
    let router = moneykeeper::bootstrap::router(&verified, Arc::new(test_jwks()));
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
    let database = test_support::fresh_database().await;
    let verified = database.initialize().await.unwrap();
    let contexts = moneykeeper::bootstrap::build_contexts(&verified);
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
    let router = moneykeeper::bootstrap::router(&verified, Arc::new(test_jwks()));
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
    let (verified, pool) = test_support::fresh_runtime().await;
    let contexts = moneykeeper::bootstrap::build_contexts(&verified);
    let user_uuid = Uuid::new_v4();
    let user = UserId::new(user_uuid);
    let mut server = TestServer::new(moneykeeper::bootstrap::router(
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
    let reversal = server
        .post(&format!("/transactions/{first_id}/reversals"))
        .add_header("Idempotency-Key", "life-reverse")
        .json(&json!({"reason":"Duplicate","occurred_at":at}))
        .await;
    assert_eq!(reversal.status_code(), StatusCode::CREATED);
    let reversal: Value = reversal.json();
    let reversed_detail: Value = server
        .get(&format!("/transactions/{first_id}"))
        .await
        .json();
    assert_eq!(
        reversed_detail["reversed_by_journal_id"],
        reversal["journal_entry_id"]
    );
    assert_eq!(
        server
            .post(&format!("/transactions/{first_id}/reversals"))
            .add_header("Idempotency-Key", "life-reverse-again")
            .json(&json!({"reason":"Again","occurred_at":at}))
            .await
            .status_code(),
        StatusCode::CONFLICT
    );
    let replacement: Value = server
        .post(&format!("/transactions/{second_id}/replacements"))
        .add_header("Idempotency-Key", "life-replace")
        .json(&json!({
            "account_id":cash_id,"kind":"expense","amount":{"amount":"7","currency":"UAH"},
            "description":"Corrected second","note":"keep this","tags":["final"],
            "budget_visibility":"excluded","occurred_at":at
        }))
        .await
        .json();
    let replacement_id = replacement["replacement_journal_entry_id"]
        .as_str()
        .unwrap();
    let replaced_detail: Value = server
        .get(&format!("/transactions/{second_id}"))
        .await
        .json();
    assert_eq!(replaced_detail["replaced_by_journal_id"], replacement_id);
    assert!(replaced_detail["reversed_by_journal_id"].is_string());
    let replacement_detail: Value = server
        .get(&format!("/transactions/{replacement_id}"))
        .await
        .json();
    assert_eq!(replacement_detail["annotation"]["note"], "keep this");
    assert_eq!(replacement_detail["annotation"]["tags"], json!(["final"]));
    assert_eq!(
        replacement_detail["annotation"]["budget_visibility"],
        "excluded"
    );
    assert_eq!(
        server
            .post(&format!("/transactions/{second_id}/replacements"))
            .add_header("Idempotency-Key", "life-replace-again")
            .json(&json!({
                "account_id":cash_id,"kind":"expense",
                "amount":{"amount":"8","currency":"UAH"},
                "description":"Another replacement","occurred_at":at
            }))
            .await
            .status_code(),
        StatusCode::CONFLICT
    );
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

const SUMMARY_RANGE: &str =
    "from_occurred_at=2026-09-01T00:00:00Z&before_occurred_at=2026-10-01T00:00:00Z";

async fn summary_category(server: &TestServer, name: &str, parent: Option<Uuid>) -> Uuid {
    let taxonomy = server.get("/categories").await.json::<Value>();
    let response = server
        .post("/categories")
        .add_header("Idempotency-Key", Uuid::new_v4().to_string())
        .json(&json!({"name":name,"kind":"both","parent_id":parent,
            "expected_version":taxonomy["version"]}))
        .await;
    response.assert_status(StatusCode::CREATED);
    response.json::<Value>()["node"]["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap()
}

async fn summary_archive(server: &TestServer, category: Uuid) {
    let taxonomy = server.get("/categories").await.json::<Value>();
    server
        .post(&format!("/categories/{category}/archive"))
        .add_header("Idempotency-Key", Uuid::new_v4().to_string())
        .json(&json!({"expected_version":taxonomy["version"]}))
        .await
        .assert_status_ok();
}

async fn summary_account(server: &TestServer, currency: &str) -> Uuid {
    let response = server
        .post("/accounts")
        .add_header("Idempotency-Key", Uuid::new_v4().to_string())
        .json(
            &json!({"name":currency,"currency":currency,"kind":"cash","nature":"asset",
            "opening_balance":"0","occurred_at":"2026-08-01T00:00:00Z"}),
        )
        .await;
    response.assert_status(StatusCode::CREATED);
    response.json::<Value>()["account"]["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap()
}

async fn summary_transaction(
    server: &TestServer,
    account: Uuid,
    category: Option<Uuid>,
    kind: &str,
    amount: &str,
    currency: &str,
    occurred_at: &str,
) -> Uuid {
    let response = server
        .post("/transactions")
        .add_header("Idempotency-Key", Uuid::new_v4().to_string())
        .json(
            &json!({"account_id":account,"category_id":category,"kind":kind,
            "amount":{"amount":amount,"currency":currency},"description":"Summary fixture",
            "occurred_at":occurred_at}),
        )
        .await;
    response.assert_status(StatusCode::CREATED);
    response.json::<Value>()["journal_entry_id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap()
}

// Exhaust the HTTP cursor, then independently aggregate the returned journal facts.
async fn assert_summary_parity(
    server: &TestServer,
    query: &str,
    limit: u32,
) -> (
    Vec<moneykeeper::contexts::ledger::public::JournalView>,
    moneykeeper::contexts::ledger::public::ActivitySummary,
) {
    use moneykeeper::contexts::ledger::public::{ActivitySummary, ActivityTotal, JournalView};
    use std::collections::{BTreeMap, HashSet};
    let mut journals = Vec::<JournalView>::new();
    let mut cursor = String::new();
    loop {
        let response = server
            .get(&format!("/transactions?{query}&limit={limit}{cursor}"))
            .await;
        response.assert_status_ok();
        let page = response.json::<Vec<JournalView>>();
        let Some(last) = page.last() else { break };
        cursor = format!(
            "&after_occurred_at={}&after_sequence={}",
            last.occurred_at
                .to_rfc3339_opts(chrono::SecondsFormat::Nanos, true),
            last.ledger_sequence
        );
        let previous_len = journals.len();
        journals.extend(page);
        assert_eq!(
            journals.iter().map(|j| j.id).collect::<HashSet<_>>().len(),
            journals.len(),
            "pagination repeated a journal after {previous_len} results"
        );
    }
    let response = server.get(&format!("/transactions/summary?{query}")).await;
    response.assert_status_ok();
    let summary = response.json::<ActivitySummary>();
    assert_eq!(summary.transaction_count, journals.len() as i64);
    let categories = journals
        .iter()
        .filter_map(|j| j.annotation.as_ref()?.category_id)
        .collect::<HashSet<_>>();
    assert_eq!(summary.category_count, categories.len() as i64);
    let mut amounts = BTreeMap::<String, Decimal>::new();
    for posting in journals.iter().flat_map(|j| &j.postings) {
        if matches!(
            posting.account_nature,
            AccountNature::Income | AccountNature::Expense
        ) {
            *amounts
                .entry(posting.currency.as_str().to_owned())
                .or_default() -= posting.signed_amount;
        }
    }
    let mut expected = amounts
        .into_iter()
        .filter(|(_, amount)| !amount.is_zero())
        .map(|(currency, amount)| ActivityTotal {
            currency: CurrencyCode::new(currency).unwrap(),
            amount,
        })
        .collect::<Vec<_>>();
    expected.sort_by(|a, b| {
        b.amount
            .abs()
            .cmp(&a.amount.abs())
            .then_with(|| a.currency.as_str().cmp(b.currency.as_str()))
    });
    assert_eq!(summary.totals, expected);
    (journals, summary)
}

#[tokio::test]
async fn category_summary_matches_complete_activity_with_archives_boundaries_and_currencies() {
    let server = app(Uuid::new_v4()).await;
    let root = summary_category(&server, "Summary parent", None).await;
    let uah = summary_account(&server, "UAH").await;
    let usd = summary_account(&server, "USD").await;
    let eur = summary_account(&server, "EUR").await;
    let start = "2026-09-01T00:00:00Z";
    let middle = "2026-09-15T12:00:00Z";
    // This assignment survives the selected leaf becoming a parent.
    let historical =
        summary_transaction(&server, uah, Some(root), "expense", "10", "UAH", start).await;
    let leaf = summary_category(&server, "Summary leaf", Some(root)).await;
    let archived = summary_category(&server, "Summary archived", Some(root)).await;
    let empty = summary_category(&server, "Summary empty", None).await;
    let leaf_expense =
        summary_transaction(&server, uah, Some(leaf), "expense", "20", "UAH", middle).await;
    let archived_income =
        summary_transaction(&server, uah, Some(archived), "income", "30", "UAH", middle).await;
    let usd_expense =
        summary_transaction(&server, usd, Some(leaf), "expense", "7", "USD", middle).await;
    let eur_income =
        summary_transaction(&server, eur, Some(leaf), "income", "7", "EUR", middle).await;
    let uncategorized =
        summary_transaction(&server, uah, None, "expense", "4", "UAH", middle).await;
    let before_start = summary_transaction(
        &server,
        uah,
        Some(leaf),
        "expense",
        "99",
        "UAH",
        "2026-08-31T23:59:59.999999Z",
    )
    .await;
    let at_end = summary_transaction(
        &server,
        uah,
        Some(leaf),
        "expense",
        "99",
        "UAH",
        "2026-10-01T00:00:00Z",
    )
    .await;
    summary_archive(&server, archived).await;

    for (filter, expected_ids, category_count) in [
        (
            format!("category_id={root}"),
            vec![
                historical,
                leaf_expense,
                archived_income,
                usd_expense,
                eur_income,
            ],
            3,
        ),
        (
            format!("category_id={leaf}"),
            vec![leaf_expense, usd_expense, eur_income],
            1,
        ),
        (format!("category_id={archived}"), vec![archived_income], 1),
        ("uncategorized=true".to_owned(), vec![uncategorized], 0),
        (format!("category_id={empty}"), vec![], 0),
    ] {
        let query = format!("{SUMMARY_RANGE}&{filter}");
        let (journals, summary) = assert_summary_parity(&server, &query, 1).await;
        let mut actual_ids = journals
            .iter()
            .map(|j| j.id.into_uuid())
            .collect::<Vec<_>>();
        let mut expected_ids = expected_ids;
        actual_ids.sort();
        expected_ids.sort();
        assert_eq!(actual_ids, expected_ids);
        assert!(!actual_ids.contains(&before_start));
        assert!(!actual_ids.contains(&at_end));
        assert_eq!(summary.category_count, category_count);
        if expected_ids.is_empty() {
            assert_eq!(
                serde_json::to_value(&summary).unwrap(),
                json!({"transaction_count":0,"category_count":0,"totals":[]})
            );
        }
        let (larger_page, larger_summary) = assert_summary_parity(&server, &query, 50).await;
        assert_eq!(journals, larger_page);
        assert_eq!(summary, larger_summary);
        let (_, explicit_all) =
            assert_summary_parity(&server, &format!("{query}&kind=all"), 2).await;
        assert_eq!(summary, explicit_all);
        for kind in ["income", "expense"] {
            let (matching, _) =
                assert_summary_parity(&server, &format!("{query}&kind={kind}"), 2).await;
            let expected = journals
                .iter()
                .filter(|j| {
                    let mut flows = std::collections::BTreeMap::<String, Decimal>::new();
                    for p in &j.postings {
                        if matches!(
                            p.account_nature,
                            AccountNature::Income | AccountNature::Expense
                        ) {
                            *flows.entry(p.currency.as_str().to_owned()).or_default() -=
                                p.signed_amount;
                        }
                    }
                    flows.values().any(|amount| {
                        if kind == "income" {
                            *amount > Decimal::ZERO
                        } else {
                            *amount < Decimal::ZERO
                        }
                    })
                })
                .cloned()
                .collect::<Vec<_>>();
            assert_eq!(matching, expected);
        }
    }
    let root_query = format!("{SUMMARY_RANGE}&category_id={root}");
    let (_, root_summary) = assert_summary_parity(&server, &root_query, 2).await;
    assert_eq!(
        root_summary
            .totals
            .iter()
            .map(|t| (t.currency.as_str(), t.amount))
            .collect::<Vec<_>>(),
        vec![("EUR", Decimal::from(7)), ("USD", Decimal::from(-7))]
    );
    summary_archive(&server, root).await;
    assert_eq!(
        assert_summary_parity(&server, &root_query, 2).await.1,
        root_summary
    );

    // Reversal journals and transfers retain the existing all/income/expense rules.
    server
        .post(&format!("/transactions/{uncategorized}/reversals"))
        .add_header("Idempotency-Key", "summary-reversal")
        .json(&json!({"reason":"Summary reversal","occurred_at":middle}))
        .await
        .assert_status(StatusCode::CREATED);
    let second_uah = summary_account(&server, "UAH").await;
    server.post("/transfers").add_header("Idempotency-Key", "summary-transfer")
        .json(&json!({"source_account_id":uah,"target_account_id":second_uah,
            "source_amount":{"amount":"1","currency":"UAH"},"target_amount":{"amount":"1","currency":"UAH"},
            "description":"No cash flow","occurred_at":middle})).await.assert_status(StatusCode::CREATED);
    for (kind, count) in [("all", 3), ("income", 1), ("expense", 1)] {
        let (_, summary) = assert_summary_parity(
            &server,
            &format!("{SUMMARY_RANGE}&uncategorized=true&kind={kind}"),
            1,
        )
        .await;
        assert_eq!(summary.transaction_count, count);
        if kind == "all" {
            assert!(summary.totals.is_empty());
        }
    }
}

#[tokio::test]
async fn category_summary_validates_filters_and_isolates_tenants() {
    let server = app(Uuid::new_v4()).await;
    let category = summary_category(&server, "Summary validation", None).await;
    let account = summary_account(&server, "UAH").await;
    summary_transaction(
        &server,
        account,
        Some(category),
        "expense",
        "3",
        "UAH",
        "2026-09-15T00:00:00Z",
    )
    .await;
    let (_, baseline) = assert_summary_parity(&server, SUMMARY_RANGE, 1).await;
    assert_eq!(baseline.transaction_count, 1);
    assert_eq!(
        assert_summary_parity(&server, &format!("{SUMMARY_RANGE}&uncategorized=false"), 1)
            .await
            .1,
        baseline
    );
    let category_query = format!("{SUMMARY_RANGE}&category_id={category}");
    assert_eq!(
        assert_summary_parity(&server, &format!("{category_query}&uncategorized=false"), 1)
            .await
            .1,
        baseline
    );
    for invalid in [
        format!("{SUMMARY_RANGE}&category_id=not-a-uuid"),
        format!("{SUMMARY_RANGE}&uncategorized=not-a-boolean"),
        format!("{category_query}&uncategorized=true"),
        "from_occurred_at=2026-10-01T00:00:00Z&before_occurred_at=2026-10-01T00:00:00Z".to_owned(),
        "from_occurred_at=2026-10-02T00:00:00Z&before_occurred_at=2026-10-01T00:00:00Z".to_owned(),
    ] {
        for path in ["/transactions", "/transactions/summary"] {
            server
                .get(&format!("{path}?{invalid}"))
                .await
                .assert_status_bad_request();
        }
    }
    let list_error = server
        .get(&format!(
            "/transactions?{category_query}&uncategorized=true"
        ))
        .await
        .json::<Value>();
    let summary_error = server
        .get(&format!(
            "/transactions/summary?{category_query}&uncategorized=true"
        ))
        .await
        .json::<Value>();
    assert_eq!(list_error, summary_error);
    for missing_dates in [
        "",
        "from_occurred_at=2026-09-01T00:00:00Z",
        "before_occurred_at=2026-10-01T00:00:00Z",
    ] {
        server
            .get(&format!(
                "/transactions/summary?{missing_dates}&category_id={category}"
            ))
            .await
            .assert_status_bad_request();
    }
    let foreign = jwt(Uuid::new_v4());
    let missing = Uuid::new_v4();
    for path in ["/transactions", "/transactions/summary"] {
        let absent = server
            .get(&format!("{path}?{SUMMARY_RANGE}&category_id={missing}"))
            .await;
        absent.assert_status_not_found();
        let other_tenant = server
            .get(&format!("{path}?{category_query}"))
            .clear_headers()
            .authorization_bearer(&foreign)
            .await;
        other_tenant.assert_status_not_found();
        assert_eq!(absent.json::<Value>(), other_tenant.json::<Value>());
    }
    let foreign_account = server
        .post("/accounts")
        .clear_headers()
        .authorization_bearer(&foreign)
        .add_header("Idempotency-Key", "foreign-summary-account")
        .json(&account_body("0", "2026-08-01T00:00:00Z"))
        .await;
    foreign_account.assert_status(StatusCode::CREATED);
    server
        .post("/transactions")
        .clear_headers()
        .authorization_bearer(&foreign)
        .add_header("Idempotency-Key", "foreign-summary-transaction")
        .json(
            &json!({"account_id":foreign_account.json::<Value>()["account"]["id"],"kind":"income",
            "amount":{"amount":"999","currency":"UAH"},"description":"Foreign",
            "occurred_at":"2026-09-15T00:00:00Z"}),
        )
        .await
        .assert_status(StatusCode::CREATED);
    assert_eq!(
        assert_summary_parity(&server, SUMMARY_RANGE, 1).await.1,
        baseline
    );
    assert_eq!(
        assert_summary_parity(&server, &category_query, 1).await.1,
        baseline
    );
    assert_eq!(
        assert_summary_parity(&server, &format!("{SUMMARY_RANGE}&uncategorized=true"), 1)
            .await
            .1
            .transaction_count,
        0
    );
}

#[tokio::test]
async fn category_summary_tracks_manual_and_automatic_assignment_changes() {
    use moneykeeper::contexts::classification::public::CategoryId;
    use moneykeeper::contexts::ledger::public::{
        ApplyCategoryAssignment, AssignmentOrigin, CategoryAssignmentDisposition, JournalEntryId,
    };
    let user = Uuid::new_v4();
    let (server, contexts) = app_with_contexts(user).await;
    let first = summary_category(&server, "Summary first", None).await;
    let second = summary_category(&server, "Summary second", None).await;
    let account = summary_account(&server, "UAH").await;
    let manual = summary_transaction(
        &server,
        account,
        None,
        "expense",
        "12.34",
        "UAH",
        "2026-09-15T00:00:00Z",
    )
    .await;
    for (version, category) in [(0, None), (1, Some(first)), (2, Some(second)), (3, None)] {
        if version > 0 {
            let mut patch = json!({"expected_version":version});
            if let Some(category) = category {
                patch["category_id"] = json!(category);
            } else {
                patch["clear_category"] = json!(true);
            }
            server
                .patch(&format!("/transactions/{manual}/annotation"))
                .add_header("Idempotency-Key", Uuid::new_v4().to_string())
                .json(&patch)
                .await
                .assert_status_ok();
        }
        for (filter, selected) in [
            (format!("category_id={first}"), Some(first)),
            (format!("category_id={second}"), Some(second)),
            ("uncategorized=true".to_owned(), None),
        ] {
            let (_, summary) =
                assert_summary_parity(&server, &format!("{SUMMARY_RANGE}&{filter}"), 1).await;
            assert_eq!(summary.transaction_count, i64::from(category == selected));
        }
    }
    let automatic = summary_transaction(
        &server,
        account,
        None,
        "expense",
        "5.67",
        "UAH",
        "2026-09-15T00:00:00Z",
    )
    .await;
    assert_eq!(
        assert_summary_parity(&server, &format!("{SUMMARY_RANGE}&uncategorized=true"), 1)
            .await
            .1
            .transaction_count,
        2
    );
    // Recurring is a trusted automatic assignment policy and needs no classifier service.
    for category in [first, second] {
        let journal = contexts
            .ledger
            .get_journal(UserId::new(user), JournalEntryId::new(automatic))
            .await
            .unwrap();
        let result = contexts
            .ledger
            .apply_category_assignment(ApplyCategoryAssignment {
                user_id: UserId::new(user),
                journal_entry_id: journal.id,
                category_id: Some(CategoryId::new(category)),
                origin: AssignmentOrigin::Recurring,
                classification_decision_id: None,
                expected_version: journal.annotation.unwrap().version,
                idempotency_key: IdempotencyKey::new(Uuid::new_v4().to_string()).unwrap(),
                correlation_id: CorrelationId::generate(),
                occurred_at: Utc::now(),
            })
            .await
            .unwrap();
        assert_eq!(result.disposition, CategoryAssignmentDisposition::Applied);
        for selected in [first, second] {
            let (_, summary) = assert_summary_parity(
                &server,
                &format!("{SUMMARY_RANGE}&category_id={selected}"),
                1,
            )
            .await;
            assert_eq!(summary.transaction_count, i64::from(category == selected));
            if category == selected {
                assert_eq!(summary.totals[0].amount, Decimal::new(-567, 2));
            }
        }
        let (remaining, _) =
            assert_summary_parity(&server, &format!("{SUMMARY_RANGE}&uncategorized=true"), 1).await;
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].id.into_uuid(), manual);
    }
}
