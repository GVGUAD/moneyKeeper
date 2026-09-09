mod analytics_support;
use analytics_support::*;
use moneykeeper::contexts::classification::public::{
    CategoryCatalog, CategoryKind, CreateCategoryNode,
};
use moneykeeper::shared_kernel::IdempotencyKey;
use serde_json::Value;
use uuid::Uuid;
const AGG: &str = "/reports/analytics?from=2026-08-01T00:00:00Z&to=2026-09-01T00:00:00Z&comparison_from=2026-07-01T00:00:00Z&comparison_to=2026-08-01T00:00:00Z&timezone=UTC&currency=UAH";
const LIST: &str = "/reports/analytics/transactions?from=2026-08-01T00:00:00Z&to=2026-09-01T00:00:00Z&timezone=UTC&currency=UAH";
#[tokio::test]
async fn authenticated_wire_contract_and_invalid_filters() {
    let f = Fixture::new().await;
    let s = server(&f).await;
    f.acceptance().await;
    let response = s.get(AGG).await;
    response.assert_status_ok();
    let body = response.json::<Value>();
    assert_eq!(body.as_object().unwrap().len(), 6);
    assert_eq!(body["current"]["expenses"], "145");
    assert_eq!(body["comparison"]["expenses"], "100");
    assert_eq!(body["series"].as_array().unwrap().len(), 31);
    assert_eq!(body["trend"].as_array().unwrap().len(), 6);
    let totals = body["current"].as_object().unwrap();
    assert_eq!(totals.len(), 8);
    assert_eq!(body["metadata"]["coverage"], "recorded_transactions");
    assert!(body["metadata"]["category_scope"].is_null());
    let response = s.get(&format!("{LIST}&kind=expense")).await;
    response.assert_status_ok();
    let list = response.json::<Value>();
    assert_eq!(list["summary"]["contribution"], "-145");
    assert_eq!(list["items"].as_array().unwrap().len(), 4);
    assert!(list["next_cursor"].is_null());
    for suffix in [
        "&foo=1",
        "&category_scope=direct",
        "&category_scope=invalid",
        "&trend_months=7",
        "&uncategorized=true&category_scope=subtree",
        "&limit=50",
        "&timezone=bad",
    ] {
        let r = s.get(&format!("{AGG}{suffix}")).await;
        r.assert_status_bad_request();
        assert!(r.json::<Value>()["error"].is_string());
    }
    for suffix in [
        "&after_sequence=1",
        "&after_occurred_at=2026-08-01T00:00:00Z",
        "&limit=0",
        "&limit=201",
        "&kind=bad",
        "&comparison_from=2026-07-01T00:00:00Z",
        "&trend_months=6",
    ] {
        s.get(&format!("{LIST}{suffix}"))
            .await
            .assert_status_bad_request();
    }
    for query in [
        AGG.replace("timezone=UTC", "timezone=Bad/Zone"),
        AGG.replace("currency=UAH", "currency=ZZZ"),
        AGG.replace("from=2026-08-01T00:00:00Z", "from=2024-01-01T00:00:00Z"),
        AGG.replace(
            "comparison_from=2026-07-01T00:00:00Z",
            "comparison_from=2024-01-01T00:00:00Z",
        ),
        AGG.replace("to=2026-09-01T00:00:00Z", "to=2026-08-01T00:00:00Z"),
    ] {
        s.get(&query).await.assert_status_bad_request();
    }
    let missing = s
        .get(&format!("{AGG}&category_id={}", Uuid::new_v4()))
        .await;
    missing.assert_status_not_found();
    assert_eq!(missing.json::<Value>()["error"], "category not found");
    let unauth = axum_test::TestServer::new(moneykeeper::api::router(
        f.contexts.clone(),
        std::sync::Arc::new(test_jwks()),
    ))
    .unwrap();
    unauth.get(AGG).await.assert_status_unauthorized();
    unauth.get(LIST).await.assert_status_unauthorized();
}
async fn category(
    f: &Fixture,
    name: &str,
    parent: Option<moneykeeper::contexts::classification::public::CategoryId>,
) -> moneykeeper::contexts::classification::public::CategoryId {
    let version = f
        .contexts
        .categories
        .taxonomy(f.user, chrono::Utc::now())
        .await
        .unwrap()
        .version;
    f.contexts
        .categories
        .create_node(
            CreateCategoryNode {
                user_id: f.user,
                idempotency_key: IdempotencyKey::new(Uuid::new_v4().to_string()).unwrap(),
                expected_version: version,
                name: name.into(),
                kind: CategoryKind::Expense,
                parent_id: parent,
                position: None,
                color: None,
                icon: None,
            },
            chrono::Utc::now(),
        )
        .await
        .unwrap()
        .node
        .category
        .id
}
async fn assign(
    f: &Fixture,
    journal: Uuid,
    category: moneykeeper::contexts::classification::public::CategoryId,
) {
    sqlx::query("INSERT INTO ledger.transaction_annotations(id,journal_entry_id,user_id,description,category_id,assignment_origin,automation_state) VALUES($1,$2,$3,'annotated',$4,'manual','suppressed') ON CONFLICT(journal_entry_id,user_id) DO UPDATE SET category_id=EXCLUDED.category_id,description=EXCLUDED.description").bind(Uuid::new_v4()).bind(journal).bind(f.user.into_uuid()).bind(category.into_uuid()).execute(&f.pool).await.unwrap();
}
#[tokio::test]
async fn taxonomy_breakdown_direct_history_archives_reassignment_and_parity() {
    let f = Fixture::new().await;
    let s = server(&f).await;
    let ids = f.acceptance().await;
    let food = category(&f, "Fixture Food", None).await;
    let groceries = category(&f, "Fixture Groceries", Some(food)).await;
    let restaurants = category(&f, "Fixture Restaurants", Some(food)).await;
    assign(&f, ids[1], groceries).await;
    assign(&f, ids[3], groceries).await;
    assign(&f, ids[4], restaurants).await;
    let body = s.get(AGG).await.json::<Value>();
    let rows = body["breakdown"].as_array().unwrap();
    let food_row = rows
        .iter()
        .find(|r| r["category_id"] == food.to_string())
        .unwrap();
    assert_eq!(food_row["current"]["expenses"], "140");
    let sum: rust_decimal::Decimal = body["series"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| {
            b["totals"]["expenses"]
                .as_str()
                .unwrap()
                .parse::<rust_decimal::Decimal>()
                .unwrap()
        })
        .sum();
    assert_eq!(sum, rust_decimal::Decimal::from(145));
    let selected = s
        .get(&format!("{AGG}&category_id={food}"))
        .await
        .json::<Value>();
    assert_eq!(selected["breakdown"].as_array().unwrap().len(), 2);
    // Historical direct-parent assignment partitions separately from descendants.
    assign(&f, ids[4], food).await;
    sqlx::query("UPDATE classification.categories SET lifecycle='archived' WHERE id=$1")
        .bind(groceries.into_uuid())
        .execute(&f.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE ledger.accounts SET lifecycle='archived' WHERE user_id=$1")
        .bind(f.user.into_uuid())
        .execute(&f.pool)
        .await
        .unwrap();
    let selected = s
        .get(&format!("{AGG}&category_id={food}"))
        .await
        .json::<Value>();
    let rows = selected["breakdown"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    assert!(
        rows.iter()
            .any(|r| r["scope"] == "direct" && r["current"]["expenses"] == "60")
    );
    assert_eq!(selected["current"]["expenses"], "140");
    let direct = s
        .get(&format!(
            "{LIST}&category_id={food}&category_scope=direct&kind=expense"
        ))
        .await
        .json::<Value>();
    assert_eq!(direct["summary"]["contribution"], "-60");
    assert_eq!(direct["items"][0]["description"], "annotated");
    let unc = s
        .get(&format!("{LIST}&uncategorized=true&kind=expense"))
        .await
        .json::<Value>();
    assert_eq!(unc["summary"]["contribution"], "-5");
    sqlx::query("UPDATE reference_data.currencies SET enabled=false WHERE code='UAH'")
        .execute(&f.pool)
        .await
        .unwrap();
    s.get(AGG).await.assert_status_ok();
}

#[tokio::test]
async fn missing_assignment_fails_even_outside_the_returned_page_and_categories_are_tenant_safe() {
    let f = Fixture::new().await;
    let s = server(&f).await;
    let other = Fixture {
        pool: f.pool.clone(),
        contexts: f.contexts.clone(),
        user: moneykeeper::shared_kernel::UserId::new(Uuid::new_v4()),
    };
    let foreign = category(&other, "Foreign", None).await;
    for path in [AGG, LIST] {
        s.get(&format!("{path}&category_id={foreign}"))
            .await
            .assert_status_not_found();
    }
    let old = f
        .journal("0", "10", "UAH", "2026-08-01T00:00:00Z", None)
        .await;
    f.journal("0", "20", "UAH", "2026-08-02T00:00:00Z", None)
        .await;
    assign(
        &f,
        old,
        moneykeeper::contexts::classification::public::CategoryId::new(Uuid::new_v4()),
    )
    .await;
    for path in [AGG.to_string(), format!("{LIST}&limit=1")] {
        s.get(&path)
            .await
            .assert_status(axum::http::StatusCode::CONFLICT);
    }
}

#[tokio::test]
async fn comparison_only_and_zero_net_expense_groups_remain_visible() {
    let f = Fixture::new().await;
    let s = server(&f).await;
    let old = category(&f, "Previous only", None).await;
    let current = category(&f, "Credits cancel purchases", None).await;
    let id = f
        .journal("0", "100", "UAH", "2026-07-10T12:00:00Z", None)
        .await;
    assign(&f, id, old).await;
    for expense in ["10", "-10"] {
        let id = f
            .journal("0", expense, "UAH", "2026-08-10T12:00:00Z", None)
            .await;
        assign(&f, id, current).await;
    }
    let r = s.get(AGG).await;
    r.assert_status_ok();
    let body = r.json::<Value>();
    let rows = body["breakdown"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["category_id"], current.to_string());
    assert_eq!(rows[0]["current"]["expenses"], "0");
    assert_eq!(rows[0]["current"]["expense_count"], 2);
    assert_eq!(rows[1]["category_id"], old.to_string());
    assert_eq!(rows[1]["current"]["expense_count"], 0);
    assert_eq!(rows[1]["comparison"]["expenses"], "100");
    let usd = s.get(&AGG.replace("currency=UAH", "currency=USD")).await;
    usd.assert_status_ok();
    assert!(
        usd.json::<Value>()["breakdown"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn spending_breakdown_contains_only_expense_bearing_journals() {
    let f = Fixture::new().await;
    let s = server(&f).await;
    let selected = category(&f, "Mixed assignments", None).await;
    for (income, expense) in [("100", "0"), ("0", "10"), ("20", "20")] {
        let id = f
            .journal(income, expense, "UAH", "2026-08-10T12:00:00Z", None)
            .await;
        assign(&f, id, selected).await;
    }
    let response = s.get(AGG).await;
    response.assert_status_ok();
    let body = response.json::<Value>();
    assert_eq!(body["current"]["income"], "120");
    assert_eq!(body["current"]["transaction_count"], 3);
    assert_eq!(body["breakdown"][0]["current"]["income"], "20");
    assert_eq!(body["breakdown"][0]["current"]["expense_count"], 2);
    assert_eq!(body["breakdown"][0]["current"]["transaction_count"], 2);
}
