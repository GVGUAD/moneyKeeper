use std::collections::BTreeSet;

use serde_json::Value;

const OPENAPI: &str = include_str!("../static/openapi.v2.json");

fn contract() -> Value {
    serde_json::from_str(OPENAPI).expect("Finance V2 OpenAPI must be valid JSON")
}

#[test]
fn openapi_v2_is_unversioned_and_has_exact_finance_routes() {
    let document = contract();
    assert_eq!(document["openapi"], "3.1.0");
    let actual: BTreeSet<&str> = document["paths"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    let expected = BTreeSet::from([
        "/currencies",
        "/currencies/{code}",
        "/categories",
        "/categories/{id}",
        "/categories/{id}/archive",
        "/categories/{id}/restore",
        "/preferences",
        "/accounts",
        "/accounts/{id}",
        "/accounts/{id}/archive",
        "/accounts/{id}/restore",
        "/accounts/{id}/activity",
        "/transactions",
        "/transactions/{id}",
        "/transactions/{id}/annotation",
        "/transactions/{id}/reversals",
        "/transactions/{id}/replacements",
        "/transfers",
        "/accounts/{id}/balance-corrections",
        "/reconciliations",
        "/reconciliations/{id}",
        "/reconciliations/{id}/approve",
        "/reconciliations/{id}/dismiss",
        "/provider-connections/monobank",
        "/provider-connections",
        "/provider-connections/{id}",
        "/provider-connections/{id}/disconnect",
        "/provider-connections/{id}/credential-replacements",
        "/provider-connections/{id}/webhook-rotations",
        "/provider-connections/{id}/resources",
        "/provider-connections/{id}/resource-mappings",
        "/provider-connections/{id}/resource-mappings/{mapping_id}/deactivations",
        "/provider-connections/{id}/resource-mappings/{mapping_id}/replacements",
        "/provider-connections/{id}/sync-jobs",
        "/sync-jobs/{id}",
        "/provider-events/{id}",
        "/accounting-processes/{id}",
        "/balance-observations/{id}",
    ]);
    assert_eq!(actual, expected);
    assert!(actual.iter().all(|path| !path.starts_with("/v2")));
}

#[test]
fn every_finance_operation_is_authenticated_and_uniquely_named() {
    let document = contract();
    let mut operation_ids = BTreeSet::new();
    let mut operation_count = 0;
    for path in document["paths"].as_object().unwrap().values() {
        for (method, operation) in path.as_object().unwrap() {
            if !["get", "post", "patch", "put", "delete"].contains(&method.as_str()) {
                continue;
            }
            operation_count += 1;
            assert_eq!(
                operation["security"][0]["bearerAuth"],
                serde_json::json!([])
            );
            assert_eq!(
                operation["responses"]["500"]["$ref"],
                "#/components/responses/InternalServerError"
            );
            assert!(
                operation["summary"]
                    .as_str()
                    .is_some_and(|summary| !summary.trim().is_empty()),
                "{method} operation is missing a summary"
            );
            assert!(
                operation["description"]
                    .as_str()
                    .is_some_and(|description| !description.trim().is_empty()),
                "{method} operation is missing a description"
            );
            let operation_id = operation["operationId"].as_str().unwrap();
            assert!(
                operation_ids.insert(operation_id),
                "duplicate {operation_id}"
            );
        }
    }
    assert_eq!(operation_count, 44);
}

#[test]
fn openapi_operations_match_the_isolated_router_manifest() {
    let document = contract();
    let actual: BTreeSet<(String, String)> = document["paths"]
        .as_object()
        .unwrap()
        .iter()
        .flat_map(|(path, item)| {
            item.as_object()
                .unwrap()
                .iter()
                .filter(|(method, _)| {
                    ["get", "post", "patch", "put", "delete"].contains(&method.as_str())
                })
                .map(move |(method, _)| (method.to_ascii_uppercase(), path.clone()))
        })
        .collect();
    let expected: BTreeSet<(String, String)> = moneykeeper::api::v2::ROUTE_MANIFEST
        .iter()
        .map(|(method, path)| ((*method).to_owned(), (*path).to_owned()))
        .collect();
    assert_eq!(actual, expected);
}

#[test]
fn optimistic_concurrency_fields_are_required() {
    let document = contract();
    for schema in [
        "RenameCategory",
        "ExpectedVersion",
        "UpdatePreferences",
        "RenameAccount",
        "ExpectedAccountVersion",
        "AnnotationUpdate",
        "ApproveReconciliation",
        "DismissReconciliation",
    ] {
        let required = document["components"]["schemas"][schema]["required"]
            .as_array()
            .unwrap();
        assert!(
            required.iter().any(|field| field == "expected_version"),
            "{schema} must require expected_version"
        );
    }
    assert!(
        document["components"]["schemas"]
            .as_object()
            .unwrap()
            .keys()
            .all(|name| !name.to_ascii_lowercase().contains("delete"))
    );
}
