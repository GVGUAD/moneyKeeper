use std::collections::BTreeSet;
#[test]
fn reporting_exposes_only_the_frozen_read_routes() {
    let routes: BTreeSet<_> = moneykeeper::api::routes::ROUTE_MANIFEST
        .iter()
        .copied()
        .filter(|(_, p)| p.starts_with("/reports/"))
        .collect();
    assert_eq!(
        routes,
        BTreeSet::from([
            ("GET", "/reports/analytics"),
            ("GET", "/reports/analytics/transactions"),
            ("GET", "/reports/balance-history"),
            ("GET", "/reports/cashflow"),
            ("GET", "/reports/spending"),
            ("GET", "/reports/liabilities"),
            ("GET", "/reports/reconciliations"),
            ("GET", "/reports/recurring"),
            ("GET", "/reports/net-worth"),
        ])
    );
    assert!(routes.iter().all(|(method, _)| *method == "GET"));
}
