use std::sync::Arc;

use chrono::{Duration, NaiveDate, Utc};

use crate::domain::fx_rate::{FxRateRepository, FxRateSource};

pub struct FxSyncUseCase {
    source: Arc<dyn FxRateSource>,
    repo: Arc<dyn FxRateRepository>,
}

impl FxSyncUseCase {
    pub fn new(source: Arc<dyn FxRateSource>, repo: Arc<dyn FxRateRepository>) -> Self {
        Self { source, repo }
    }

    pub async fn sync_today(&self) -> anyhow::Result<usize> {
        self.sync_date(Utc::now().date_naive()).await
    }

    pub async fn sync_date(&self, date: NaiveDate) -> anyhow::Result<usize> {
        let rates = self.source.fetch_rates_for(date).await?;
        let n = rates.len();
        self.repo.upsert_many(&rates).await?;
        Ok(n)
    }

    pub async fn backfill(&self, from: NaiveDate, to: NaiveDate) -> anyhow::Result<usize> {
        let mut total = 0;
        let mut d = from;
        while d <= to {
            total += self.sync_date(d).await?;
            d += Duration::days(1);
        }
        Ok(total)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::fx_rate::FxRate;
    use async_trait::async_trait;
    use rust_decimal::Decimal;
    use std::sync::Mutex;

    #[derive(Default)]
    struct FakeSource {
        calls: Mutex<Vec<NaiveDate>>,
    }

    #[async_trait]
    impl FxRateSource for FakeSource {
        async fn fetch_rates_for(&self, date: NaiveDate) -> anyhow::Result<Vec<FxRate>> {
            self.calls.lock().unwrap().push(date);
            Ok(vec![FxRate {
                rate_date: date,
                from_currency: "USD".to_string(),
                to_currency: "UAH".to_string(),
                rate: Decimal::new(40, 0),
            }])
        }
    }

    #[derive(Default)]
    struct FakeRepo {
        upserts: Mutex<Vec<FxRate>>,
    }

    #[async_trait]
    impl FxRateRepository for FakeRepo {
        async fn rate_as_of(
            &self,
            _date: NaiveDate,
            _from: &str,
            _to: &str,
        ) -> anyhow::Result<Option<Decimal>> {
            unimplemented!()
        }
        async fn upsert_many(&self, rates: &[FxRate]) -> anyhow::Result<()> {
            self.upserts.lock().unwrap().extend(rates.iter().cloned());
            Ok(())
        }
        async fn latest_date(&self) -> anyhow::Result<Option<NaiveDate>> {
            unimplemented!()
        }
        async fn known_currencies(&self) -> anyhow::Result<Vec<String>> {
            unimplemented!()
        }
    }

    #[tokio::test]
    async fn sync_date_fetches_and_upserts() {
        let source = Arc::new(FakeSource::default());
        let repo = Arc::new(FakeRepo::default());
        let usecase = FxSyncUseCase::new(source.clone(), repo.clone());

        let date = NaiveDate::from_ymd_opt(2026, 5, 10).unwrap();
        let n = usecase.sync_date(date).await.unwrap();

        assert_eq!(n, 1);
        assert_eq!(*source.calls.lock().unwrap(), vec![date]);
        assert_eq!(repo.upserts.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn backfill_calls_source_for_each_date_inclusive() {
        let source = Arc::new(FakeSource::default());
        let repo = Arc::new(FakeRepo::default());
        let usecase = FxSyncUseCase::new(source.clone(), repo.clone());

        let from = NaiveDate::from_ymd_opt(2026, 5, 8).unwrap();
        let to = NaiveDate::from_ymd_opt(2026, 5, 10).unwrap();
        usecase.backfill(from, to).await.unwrap();

        assert_eq!(source.calls.lock().unwrap().len(), 3);
    }
}
