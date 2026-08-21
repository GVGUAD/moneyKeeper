mod v2_test_support;

use sqlx::Row;
use uuid::Uuid;

#[tokio::test]
async fn schema_installs_tenant_safe_immutable_loan_storage() {
    let (_verified, pool) = v2_test_support::fresh_v2_runtime().await;
    for table in [
        "agreements",
        "term_revisions",
        "component_balances",
        "movements",
        "movement_status_history",
        "replacement_processes",
        "reversal_requests",
        "command_receipts",
        "audit_log",
    ] {
        let exists: bool = sqlx::query_scalar("SELECT to_regclass($1) IS NOT NULL")
            .bind(format!("loans.{table}"))
            .fetch_one(&pool)
            .await
            .unwrap();
        assert!(exists, "missing loans.{table}");
    }
    let user = Uuid::new_v4();
    let agreement = Uuid::new_v4();
    let now = chrono::Utc::now();
    sqlx::query("INSERT INTO loans.agreements(id,user_id,direction,counterparty,contractual_principal,currency,start_date,status,version,created_at,updated_at) VALUES($1,$2,'borrowed','Alex',1000,'UAH',CURRENT_DATE,'pending_accounting',1,$3,$3)")
        .bind(agreement).bind(user).bind(now).execute(&pool).await.unwrap();
    let revision = Uuid::new_v4();
    sqlx::query("INSERT INTO loans.term_revisions(id,agreement_id,user_id,revision,counterparty,contractual_principal,start_date,reason,recorded_at) VALUES($1,$2,$3,1,'Alex',1000,CURRENT_DATE,'Agreement opened',$4)")
        .bind(revision).bind(agreement).bind(user).bind(now).execute(&pool).await.unwrap();
    assert!(
        sqlx::query("UPDATE loans.term_revisions SET reason='rewritten' WHERE id=$1")
            .bind(revision)
            .execute(&pool)
            .await
            .is_err()
    );
    let cross_user = Uuid::new_v4();
    let error=sqlx::query("INSERT INTO loans.component_balances(agreement_id,user_id,currency,updated_at) VALUES($1,$2,'UAH',$3)").bind(agreement).bind(cross_user).bind(now).execute(&pool).await.unwrap_err();
    assert!(error.as_database_error().is_some());
}

#[tokio::test]
async fn schema_rejects_negative_confirmed_components_and_mutating_posted_facts() {
    let (_verified, pool) = v2_test_support::fresh_v2_runtime().await;
    let user = Uuid::new_v4();
    let agreement = Uuid::new_v4();
    let now = chrono::Utc::now();
    sqlx::query("INSERT INTO loans.agreements(id,user_id,direction,counterparty,contractual_principal,currency,start_date,ledger_principal_account_id,status,version,created_at,updated_at) VALUES($1,$2,'lent','Alex',1000,'UAH',CURRENT_DATE,$3,'active',2,$4,$4)").bind(agreement).bind(user).bind(Uuid::new_v4()).bind(now).execute(&pool).await.unwrap();
    assert!(sqlx::query("INSERT INTO loans.component_balances(agreement_id,user_id,currency,principal,updated_at) VALUES($1,$2,'UAH',-1,$3)").bind(agreement).bind(user).bind(now).execute(&pool).await.is_err());
    sqlx::query("INSERT INTO loans.component_balances(agreement_id,user_id,currency,updated_at) VALUES($1,$2,'UAH',$3)").bind(agreement).bind(user).bind(now).execute(&pool).await.unwrap();
    let movement = Uuid::new_v4();
    sqlx::query("INSERT INTO loans.movements(id,agreement_id,user_id,sequence,kind,currency,principal,cash_account_id,status,process_correlation_id,ledger_journal_id,requested_at,posted_at) VALUES($1,$2,$3,1,'disbursement','UAH',100,$4,'posted',$5,$6,$7,$7)").bind(movement).bind(agreement).bind(user).bind(Uuid::new_v4()).bind(Uuid::new_v4()).bind(Uuid::new_v4()).bind(now).execute(&pool).await.unwrap();
    assert!(
        sqlx::query("UPDATE loans.movements SET principal=99 WHERE id=$1")
            .bind(movement)
            .execute(&pool)
            .await
            .is_err()
    );
    let status: String = sqlx::query("SELECT status FROM loans.movements WHERE id=$1")
        .bind(movement)
        .fetch_one(&pool)
        .await
        .unwrap()
        .get("status");
    assert_eq!(status, "posted");
}
