mod test_support;

use std::collections::BTreeSet;
use std::sync::Arc;

use axum_test::TestServer;
use jsonwebtoken::jwk::JwkSet;
use moneykeeper::api::routes;

#[test]
fn default_router_manifest_is_exhaustive_and_unversioned() {
    let manifest: BTreeSet<_> = routes::ROUTE_MANIFEST.iter().copied().collect();
    assert_eq!(manifest.len(), routes::ROUTE_MANIFEST.len());
    assert!(manifest.iter().all(|(_, path)| !path.starts_with("/v2")));
}

#[tokio::test]
async fn removed_legacy_mutations_and_versioned_aliases_are_not_found() {
    let pool = test_support::fresh_pool().await;
    let jwks: JwkSet = serde_json::from_value(serde_json::json!({"keys": []})).unwrap();
    let server = TestServer::new(moneykeeper::bootstrap::router(&pool, Arc::new(jwks))).unwrap();
    let id = uuid::Uuid::new_v4();

    for response in [
        server.delete(&format!("/accounts/{id}")).await,
        server.delete(&format!("/transactions/{id}")).await,
        server.patch(&format!("/accounts/{id}/balance")).await,
        server.post("/webhooks/monobank").await,
        server.get("/v2/accounts").await,
    ] {
        assert_eq!(response.status_code(), reqwest::StatusCode::NOT_FOUND);
    }
}
