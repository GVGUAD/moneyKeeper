use std::collections::BTreeSet;
#[test]
fn isolated_router_and_openapi_publish_exact_portfolio_paths() {
    let manifest: BTreeSet<_> = moneykeeper::api::v2::ROUTE_MANIFEST
        .iter()
        .copied()
        .collect();
    for route in [
        ("POST", "/portfolio-accounts"),
        ("GET", "/portfolio-accounts"),
        ("PATCH", "/portfolio-accounts/{id}"),
        ("POST", "/instruments/ovdp"),
        ("POST", "/portfolio-transactions"),
        ("POST", "/portfolio-transactions/{id}/reversals"),
        ("GET", "/portfolio-positions"),
        ("POST", "/valuations"),
    ] {
        assert!(manifest.contains(&route));
    }
    let document: serde_json::Value =
        serde_json::from_str(include_str!("../static/openapi.v2.json")).unwrap();
    assert!(
        document["paths"]
            .as_object()
            .unwrap()
            .keys()
            .all(|p| !p.starts_with("/v2"))
    );
}
#[test]
fn every_portfolio_mutation_documents_idempotency() {
    let d: serde_json::Value =
        serde_json::from_str(include_str!("../static/openapi.v2.json")).unwrap();
    for (path, method) in [
        ("/portfolio-accounts", "post"),
        ("/portfolio-accounts/{id}", "patch"),
        ("/portfolio-accounts/{id}/archive", "post"),
        ("/portfolio-accounts/{id}/restore", "post"),
        ("/instruments/ovdp", "post"),
        ("/portfolio-transactions", "post"),
        ("/portfolio-transactions/{id}/reversals", "post"),
        ("/valuations", "post"),
    ] {
        assert!(
            d["paths"][path][method]["parameters"]
                .as_array()
                .is_some_and(|v| v
                    .iter()
                    .any(|p| p["$ref"] == "#/components/parameters/IdempotencyKey")),
            "{method} {path}"
        );
    }
}
