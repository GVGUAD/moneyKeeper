mod v2_test_support;
use sqlx::Row;
#[tokio::test]
async fn schema_mail_is_encrypted_only_and_facts_are_immutable() {
    let (_verified, pool) = v2_test_support::fresh_v2_runtime().await;
    let columns: Vec<String> = sqlx::query("SELECT column_name FROM information_schema.columns WHERE table_schema='mail' AND table_name IN ('connections','oauth_states')")
        .fetch_all(&pool).await.unwrap().into_iter().map(|r|r.get("column_name")).collect();
    assert!(columns.iter().any(|c| c == "credential_ciphertext"));
    assert!(columns.iter().any(|c| c == "verifier_ciphertext"));
    assert!(
        !columns
            .iter()
            .any(|c| matches!(c.as_str(), "access_token" | "refresh_token" | "verifier"))
    );
}
