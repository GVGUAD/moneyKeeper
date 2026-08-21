use chrono::Utc;
use moneykeeper::{
    contexts::reference_data::public::{ExchangeRate, FxError},
    shared_kernel::CurrencyCode,
};
use rust_decimal_macros::dec;
#[path = "v2_test_support.rs"]
mod v2_test_support;
#[test]
fn exchange_rates_are_positive_directional_and_exact() {
    let usd = CurrencyCode::new("USD").unwrap();
    let uah = CurrencyCode::new("UAH").unwrap();
    assert_eq!(
        ExchangeRate::new(usd.clone(), uah.clone(), dec!(0)),
        Err(FxError::NonPositive)
    );
    let rate = ExchangeRate::new(usd.clone(), uah.clone(), dec!(41.25)).unwrap();
    let inverse = rate.invert().unwrap();
    assert_eq!(inverse.base(), &uah);
    assert_eq!(inverse.quote(), &usd);
    assert!(inverse.rate() > rust_decimal::Decimal::ZERO);
}
#[test]
fn triangulation_requires_a_currency_chain() {
    let usd = CurrencyCode::new("USD").unwrap();
    let uah = CurrencyCode::new("UAH").unwrap();
    let eur = CurrencyCode::new("EUR").unwrap();
    let a = ExchangeRate::new(usd.clone(), uah.clone(), dec!(41.25)).unwrap();
    let b = ExchangeRate::new(uah, eur.clone(), dec!(0.022)).unwrap();
    let cross = a.triangulate(&b).unwrap();
    assert_eq!(cross.base(), &usd);
    assert_eq!(cross.quote(), &eur);
    assert_eq!(cross.rate(), dec!(0.90750));
}

#[tokio::test]
async fn immutable_observations_replay_and_cross_through_uah() {
    use moneykeeper::contexts::reference_data::public::RecordFxObservation;
    let (verified, _pool) = v2_test_support::fresh_v2_runtime().await;
    let currencies = moneykeeper::bootstrap::v2::supporting_contexts(&verified).currencies;
    let now = Utc::now();
    for (code, rate) in [("USD", dec!(40)), ("EUR", dec!(50))] {
        let command = RecordFxObservation {
            source: "test".into(),
            source_revision: format!("today:{code}"),
            rate: ExchangeRate::new(
                CurrencyCode::new(code).unwrap(),
                CurrencyCode::new("UAH").unwrap(),
                rate,
            )
            .unwrap(),
            effective_at: now,
            observed_at: now,
            recorded_at: now,
            content_digest: [code.as_bytes()[0]; 32],
        };
        let first = currencies
            .record_fx_observation(command.clone())
            .await
            .unwrap();
        let replay = currencies.record_fx_observation(command).await.unwrap();
        assert_eq!(first.observation_id, replay.observation_id);
        assert!(replay.replayed);
    }
    let cross = currencies
        .rate_as_of(
            CurrencyCode::new("USD").unwrap(),
            CurrencyCode::new("EUR").unwrap(),
            now,
        )
        .await
        .unwrap();
    assert_eq!(cross.rate, dec!(0.8));
    assert!(matches!(
        cross.derivation,
        moneykeeper::contexts::reference_data::public::FxDerivation::Cross { .. }
    ));
}
