use std::collections::BTreeSet;
#[test]
fn reporting_exposes_only_the_frozen_read_routes() {
    let routes: BTreeSet<_> = moneykeeper::api::v2::ROUTE_MANIFEST
        .iter()
        .copied()
        .filter(|(_, p)| p.starts_with("/reports/"))
        .collect();
    assert_eq!(routes.len(), 7);
    assert!(routes.iter().all(|(method, _)| *method == "GET"));
}
