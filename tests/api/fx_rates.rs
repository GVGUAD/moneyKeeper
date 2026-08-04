use chrono::NaiveDate;
use moneykeeper::domain::fx_rate::{FxRate, FxRateRepository};
use moneykeeper::infrastructure::fx_rate_repository::PgFxRateRepository;
use rust_decimal::Decimal;

use super::{common, helpers};

#[tokio::test]
async fn rate_as_of_returns_exact_date_rate() {
    let db = common::TestPostgres::new().await;
    let date = NaiveDate::from_ymd_opt(2026, 5, 10).unwrap();
    helpers::seed_fx_rate(&db.pool, date, "USD", Decimal::new(405, 1)).await;

    let repo = PgFxRateRepository::new(db.pool.clone());
    let rate = repo.rate_as_of(date, "USD", "UAH").await.unwrap();

    assert_eq!(rate, Some(Decimal::new(405, 1)));
}

#[tokio::test]
async fn rate_as_of_falls_back_to_earlier_date() {
    let db = common::TestPostgres::new().await;
    let earlier = NaiveDate::from_ymd_opt(2026, 5, 1).unwrap();
    let query_date = NaiveDate::from_ymd_opt(2026, 5, 10).unwrap();
    helpers::seed_fx_rate(&db.pool, earlier, "USD", Decimal::new(400, 1)).await;

    let repo = PgFxRateRepository::new(db.pool.clone());
    let rate = repo.rate_as_of(query_date, "USD", "UAH").await.unwrap();

    assert_eq!(rate, Some(Decimal::new(400, 1)));
}

#[tokio::test]
async fn rate_as_of_returns_none_when_no_rate_exists() {
    let db = common::TestPostgres::new().await;
    let repo = PgFxRateRepository::new(db.pool.clone());

    let rate = repo
        .rate_as_of(NaiveDate::from_ymd_opt(2026, 5, 10).unwrap(), "USD", "UAH")
        .await
        .unwrap();

    assert_eq!(rate, None);
}

#[tokio::test]
async fn rate_as_of_identity_for_same_currency() {
    let db = common::TestPostgres::new().await;
    let repo = PgFxRateRepository::new(db.pool.clone());

    let rate = repo
        .rate_as_of(NaiveDate::from_ymd_opt(2026, 5, 10).unwrap(), "UAH", "UAH")
        .await
        .unwrap();

    assert_eq!(rate, Some(Decimal::ONE));
}

#[tokio::test]
async fn rate_as_of_cross_rate_via_uah() {
    let db = common::TestPostgres::new().await;
    let date = NaiveDate::from_ymd_opt(2026, 5, 10).unwrap();
    helpers::seed_fx_rate(&db.pool, date, "USD", Decimal::new(40, 0)).await;
    helpers::seed_fx_rate(&db.pool, date, "EUR", Decimal::new(50, 0)).await;

    let repo = PgFxRateRepository::new(db.pool.clone());
    let rate = repo.rate_as_of(date, "USD", "EUR").await.unwrap();

    assert_eq!(rate, Some(Decimal::new(8, 1)));
}

#[tokio::test]
async fn upsert_many_replaces_existing() {
    let db = common::TestPostgres::new().await;
    let date = NaiveDate::from_ymd_opt(2026, 5, 10).unwrap();
    helpers::seed_fx_rate(&db.pool, date, "USD", Decimal::new(40, 0)).await;

    let repo = PgFxRateRepository::new(db.pool.clone());
    repo.upsert_many(&[FxRate {
        rate_date: date,
        from_currency: "USD".to_string(),
        to_currency: "UAH".to_string(),
        rate: Decimal::new(45, 0),
    }])
    .await
    .unwrap();

    let rate = repo.rate_as_of(date, "USD", "UAH").await.unwrap();
    assert_eq!(rate, Some(Decimal::new(45, 0)));
}
