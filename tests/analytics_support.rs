#![allow(dead_code)]
use chrono::{DateTime, Utc};
use moneykeeper::{
    bootstrap::ContextFacades,
    contexts::ledger::public::*,
    shared_kernel::{CurrencyCode, UserId},
};
use rust_decimal::Decimal;
use sqlx::PgPool;
use uuid::Uuid;
#[path = "test_support.rs"]
mod test_support;
pub fn at(s: &str) -> DateTime<Utc> {
    s.parse().unwrap()
}
pub fn range() -> AnalyticsInterval {
    AnalyticsInterval {
        from: at("2026-08-01T00:00:00Z"),
        to: at("2026-09-01T00:00:00Z"),
    }
}
pub fn filter() -> AnalyticsFilter {
    AnalyticsFilter {
        currency: CurrencyCode::new("UAH").unwrap(),
        categories: AnalyticsCategories::All,
    }
}
pub struct Fixture {
    pub pool: PgPool,
    pub contexts: ContextFacades,
    pub user: UserId,
}
impl Fixture {
    pub async fn new() -> Self {
        let (db, pool) = test_support::fresh_runtime().await;
        Self {
            pool,
            contexts: moneykeeper::bootstrap::build_contexts(&db),
            user: UserId::new(Uuid::new_v4()),
        }
    }
    pub async fn journal(
        &self,
        income: &str,
        expenses: &str,
        currency: &str,
        occurred: &str,
        relation: Option<(Uuid, bool)>,
    ) -> Uuid {
        let income: Decimal = income.parse().unwrap();
        let expenses: Decimal = expenses.parse().unwrap();
        let id = Uuid::new_v4();
        let mut tx = self.pool.begin().await.unwrap();
        let purpose = if relation.is_some_and(|(_, reverse)| reverse) {
            "reversal"
        } else {
            "ordinary"
        };
        sqlx::query("INSERT INTO ledger.journal_entries(id,user_id,command_name,source,purpose,description,actor_kind,occurred_at,recorded_at,correlation_id,idempotency_key,reverses_transaction_id,replaces_transaction_id) VALUES($1,$2,'analytics_fixture',$3,$4,'fixture','system',$5,now(),$1,$6,$7,$8)")
  .bind(id).bind(self.user.into_uuid()).bind(if relation.is_some(){"correction"}else{"manual"}).bind(purpose).bind(at(occurred)).bind(id.to_string()).bind(relation.filter(|(_,r)|*r).map(|(id,_)|id)).bind(relation.filter(|(_,r)|!*r).map(|(id,_)|id)).execute(&mut *tx).await.unwrap();
        let mut postings = vec![
            ("income", "uncategorized_income", -income),
            ("expense", "uncategorized_expense", expenses),
            ("asset", "fx_clearing", income - expenses),
        ];
        // A zero-component fixture is a pure principal transfer.
        if income.is_zero() && expenses.is_zero() {
            postings = vec![
                ("asset", "fx_clearing", Decimal::ONE),
                ("equity", "opening_balance_equity", -Decimal::ONE),
            ];
        }
        let mut pos = 0_i16;
        for (nature, role, amount) in postings {
            if amount.is_zero() {
                continue;
            }
            pos += 1;
            let account:Uuid=sqlx::query_scalar("INSERT INTO ledger.accounts(id,user_id,name,currency,nature,kind,authority,visibility,system_role) VALUES($1,$2,$3,$4,$3,'system','system','hidden',$5) ON CONFLICT (user_id,system_role,(COALESCE(system_subject_reference,'')),currency) WHERE authority='system' DO UPDATE SET name=EXCLUDED.name RETURNING id").bind(Uuid::new_v4()).bind(self.user.into_uuid()).bind(nature).bind(currency).bind(role).fetch_one(&mut *tx).await.unwrap();
            sqlx::query("INSERT INTO ledger.postings(id,journal_entry_id,user_id,account_id,currency,account_nature,position,signed_amount) VALUES($1,$2,$3,$4,$5,$6,$7,$8)").bind(Uuid::new_v4()).bind(id).bind(self.user.into_uuid()).bind(account).bind(currency).bind(nature).bind(pos).bind(amount).execute(&mut *tx).await.unwrap();
        }
        tx.commit().await.unwrap();
        id
    }
    pub async fn acceptance(&self) -> Vec<Uuid> {
        let mut ids = vec![];
        for (i, e) in [("1200", "0"), ("0", "100"), ("0", "5"), ("0", "-20")] {
            ids.push(
                self.journal(i, e, "UAH", "2026-08-10T12:00:00Z", None)
                    .await,
            );
        }
        let old = self
            .journal("0", "80", "UAH", "2026-08-10T12:00:00Z", None)
            .await;
        ids.push(
            self.journal("0", "60", "UAH", "2026-08-10T12:00:00Z", Some((old, false)))
                .await,
        );
        let undone = self
            .journal("0", "40", "UAH", "2026-08-10T12:00:00Z", None)
            .await;
        self.journal(
            "0",
            "-40",
            "UAH",
            "2026-09-05T12:00:00Z",
            Some((undone, true)),
        )
        .await;
        self.journal("0", "0", "UAH", "2026-08-10T12:00:00Z", None)
            .await;
        self.journal("0", "30", "USD", "2026-08-10T12:00:00Z", None)
            .await;
        self.journal("0", "100", "UAH", "2026-07-10T12:00:00Z", None)
            .await;
        ids
    }
}

use serde_json::json;
const TEST_KID: &str = "test-key-1";
const TEST_EC_PRIVATE_KEY: &str = "-----BEGIN PRIVATE KEY-----
MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgN/zCeuQq48O/tp5y
b50qbJqKns6bCt6JctzISKw2sPqhRANCAASdCO4KOpDloBoTURgT/ZeiWey7OSIG
46TJaP8IugkOaxHZ6HCuZvK4AaDOXLZHOyRHEWK5AhPl1f98M4xYBkQy
-----END PRIVATE KEY-----";

pub fn test_jwks() -> jsonwebtoken::jwk::JwkSet {
    serde_json::from_value(json!({"keys": [{
        "kty": "EC", "crv": "P-256", "kid": TEST_KID, "alg": "ES256", "use": "sig",
        "x": "nQjuCjqQ5aAaE1EYE_2XolnsuzkiBuOkyWj_CLoJDms",
        "y": "EdnocK5m8rgBoM5ctkc7JEcRYrkCE-XV_3wzjFgGRDI"
    }]}))
    .unwrap()
}

pub fn jwt(user_id: Uuid) -> String {
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

pub async fn server(f: &Fixture) -> axum_test::TestServer {
    let mut s = axum_test::TestServer::new(moneykeeper::api::router(
        f.contexts.clone(),
        std::sync::Arc::new(test_jwks()),
    ))
    .unwrap();
    s.add_header(
        axum::http::header::AUTHORIZATION,
        format!("Bearer {}", jwt(f.user.into_uuid())),
    );
    s
}
