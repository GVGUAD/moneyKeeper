use std::collections::BTreeSet;
#[test]
fn recurring_router_manifest_is_exact() {
    let routes: BTreeSet<_> = moneykeeper::api::v2::ROUTE_MANIFEST
        .iter()
        .copied()
        .filter(|(_, p)| p.starts_with("/subscription"))
        .collect();
    assert_eq!(routes.len(), 8);
    assert!(
        !routes
            .iter()
            .any(|(_, p)| p.contains("transaction_id") || p.starts_with("/v2"))
    );
}
