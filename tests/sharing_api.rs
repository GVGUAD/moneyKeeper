use std::collections::BTreeSet;

#[test]
fn isolated_router_and_openapi_publish_exact_sharing_paths() {
    let manifest: BTreeSet<_> = moneykeeper::api::v2::ROUTE_MANIFEST
        .iter()
        .copied()
        .collect();
    for route in [
        ("POST", "/contacts"),
        ("GET", "/contacts"),
        ("GET", "/contacts/{id}"),
        ("PATCH", "/contacts/{id}"),
        ("POST", "/contacts/{id}/archive"),
        ("POST", "/bill-splits"),
        ("GET", "/bill-splits"),
        ("GET", "/bill-splits/{id}"),
        ("POST", "/bill-splits/{id}/revisions"),
        ("POST", "/bill-splits/{id}/settlements"),
        (
            "POST",
            "/bill-splits/{id}/settlements/{settlement_id}/reversal",
        ),
        ("POST", "/bill-splits/{id}/cancellations"),
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
            .all(|path| !path.starts_with("/v2"))
    );
}

#[test]
fn every_sharing_mutation_documents_idempotency() {
    let document: serde_json::Value =
        serde_json::from_str(include_str!("../static/openapi.v2.json")).unwrap();
    for (path, method) in [
        ("/contacts", "post"),
        ("/contacts/{id}", "patch"),
        ("/contacts/{id}/archive", "post"),
        ("/bill-splits", "post"),
        ("/bill-splits/{id}/revisions", "post"),
        ("/bill-splits/{id}/settlements", "post"),
        (
            "/bill-splits/{id}/settlements/{settlement_id}/reversal",
            "post",
        ),
        ("/bill-splits/{id}/cancellations", "post"),
    ] {
        assert!(
            document["paths"][path][method]["parameters"]
                .as_array()
                .unwrap()
                .iter()
                .any(|parameter| parameter["$ref"] == "#/components/parameters/IdempotencyKey")
        );
    }
}
