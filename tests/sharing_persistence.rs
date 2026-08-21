mod v2_test_support;
use sqlx::Row;

#[tokio::test]
async fn schema_installs_tenant_safe_append_only_sharing_storage() {
    let (_verified, pool) = v2_test_support::fresh_v2_runtime().await;
    for table in [
        "contacts",
        "bills",
        "bill_revisions",
        "contributions",
        "participant_shares",
        "obligations",
        "settlements",
        "settlement_reversals",
        "command_receipts",
    ] {
        let exists: bool = sqlx::query_scalar("SELECT to_regclass($1) IS NOT NULL")
            .bind(format!("sharing.{table}"))
            .fetch_one(&pool)
            .await
            .unwrap();
        assert!(exists, "missing sharing.{table}");
    }
    let columns=sqlx::query("SELECT column_name FROM information_schema.columns WHERE table_schema='sharing' AND table_name='contacts'").fetch_all(&pool).await.unwrap();
    assert!(!columns.iter().any(|row| {
        row.get::<String, _>("column_name")
            .contains("application_user")
    }));
}

#[tokio::test]
async fn schema_rejects_mutating_active_allocation_facts() {
    let (_verified, pool) = v2_test_support::fresh_v2_runtime().await;
    let user = uuid::Uuid::new_v4();
    let bill = uuid::Uuid::new_v4();
    let correlation = uuid::Uuid::new_v4();
    sqlx::query("INSERT INTO sharing.bills(id,user_id,currency,status) VALUES($1,$2,'UAH','pending_accounting')").bind(bill).bind(user).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO sharing.bill_revisions(bill_id,user_id,revision,title,occurred_at,total,currency,accounting_status,accounting_correlation_id) VALUES($1,$2,1,'Dinner',clock_timestamp(),100,'UAH','pending',$3)").bind(bill).bind(user).bind(correlation).execute(&pool).await.unwrap();
    let error = sqlx::query(
        "UPDATE sharing.bill_revisions SET title='Changed' WHERE bill_id=$1 AND user_id=$2",
    )
    .bind(bill)
    .bind(user)
    .execute(&pool)
    .await
    .unwrap_err();
    assert!(error.to_string().contains("immutable"));
}
