use moneykeeper::bootstrap::v2::supporting_contexts;
use moneykeeper::contexts::reference_data::public::CurrencyCatalog;
use moneykeeper::shared_kernel::CurrencyCode;

#[path = "v2_test_support.rs"]
mod v2_test_support;

#[tokio::test]
async fn enabled_currency_lookup_and_ordering_are_public_contracts() {
    let verified = v2_test_support::fresh_v2_pool().await;
    let catalog = supporting_contexts(&verified).currencies;

    let uah = catalog
        .require_enabled(CurrencyCode::new("UAH").unwrap())
        .await
        .unwrap();
    assert_eq!(uah.code.as_str(), "UAH");
    assert_eq!(uah.minor_unit, 2);

    let definitions = catalog.list_enabled().await.unwrap();
    let codes: Vec<_> = definitions
        .iter()
        .map(|definition| definition.code.as_str())
        .collect();
    assert!(codes.windows(2).all(|pair| pair[0] < pair[1]));
    assert!(codes.contains(&"UAH"));
    assert!(codes.contains(&"USD"));
    assert!(codes.contains(&"EUR"));
}

#[tokio::test]
async fn disabled_and_missing_currencies_have_distinct_public_errors() {
    let verified = v2_test_support::fresh_v2_pool().await;
    let mut connection = verified.acquire().await.unwrap();
    sqlx::query("UPDATE reference_data.currencies SET enabled = false WHERE code = 'USD'")
        .execute(&mut *connection)
        .await
        .unwrap();
    drop(connection);
    let catalog = supporting_contexts(&verified).currencies;

    let disabled = catalog
        .require_enabled(CurrencyCode::new("USD").unwrap())
        .await
        .unwrap_err();
    assert!(disabled.is_disabled());

    let missing = catalog
        .require_enabled(CurrencyCode::new("GBP").unwrap())
        .await
        .unwrap_err();
    assert!(missing.is_not_found());
}

#[tokio::test]
async fn database_errors_do_not_expose_sql() {
    let verified = v2_test_support::fresh_v2_pool().await;
    let catalog = supporting_contexts(&verified).currencies;
    let mut connection = verified.acquire().await.unwrap();
    sqlx::query("DROP SCHEMA reference_data CASCADE")
        .execute(&mut *connection)
        .await
        .unwrap();
    drop(connection);

    let error = catalog.list_enabled().await.unwrap_err().to_string();
    assert_eq!(error, "currency catalog is unavailable");
    assert!(!error.to_ascii_lowercase().contains("select"));
    assert!(!error.contains("reference_data.currencies"));
}
