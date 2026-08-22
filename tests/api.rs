mod v2_test_support;

use std::collections::BTreeSet;
use std::sync::Arc;

use axum_test::TestServer;
use jsonwebtoken::jwk::JwkSet;
use moneykeeper::api::{routes, v2};

#[test]
fn default_router_delegates_to_the_exhaustive_v2_manifest() {
    let default: BTreeSet<_> = routes::ROUTE_MANIFEST.iter().copied().collect();
    let validated: BTreeSet<_> = v2::ROUTE_MANIFEST.iter().copied().collect();
    assert_eq!(default, validated);
    assert!(default.iter().all(|(_, path)| !path.starts_with("/v2")));
}

#[tokio::test]
async fn removed_legacy_mutations_and_versioned_aliases_are_not_found() {
    let pool = v2_test_support::fresh_v2_pool().await;
    let jwks: JwkSet = serde_json::from_value(serde_json::json!({"keys": []})).unwrap();
    let server =
        TestServer::new(moneykeeper::bootstrap::v2::router(&pool, Arc::new(jwks))).unwrap();
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
