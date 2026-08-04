use chrono::NaiveDate;
use rust_decimal::Decimal;

#[derive(Debug, Clone, PartialEq)]
pub struct FxRate {
    pub rate_date: NaiveDate,
    pub from_currency: String,
    pub to_currency: String,
    pub rate: Decimal,
}

#[async_trait::async_trait]
pub trait FxRateRepository: Send + Sync {
    /// Returns the rate for `from -> to` as of `date`, falling back to the
    /// most recent earlier rate. Returns `None` if no rate exists at all.
    /// Currencies are case-insensitive 3-letter codes.
    async fn rate_as_of(
        &self,
        date: NaiveDate,
        from: &str,
        to: &str,
    ) -> anyhow::Result<Option<Decimal>>;

    async fn upsert_many(&self, rates: &[FxRate]) -> anyhow::Result<()>;

    async fn latest_date(&self) -> anyhow::Result<Option<NaiveDate>>;

    /// Distinct `from_currency` values present in the table.
    async fn known_currencies(&self) -> anyhow::Result<Vec<String>>;
}

#[async_trait::async_trait]
pub trait FxRateSource: Send + Sync {
    /// Fetch all available rates against UAH for `date`.
    async fn fetch_rates_for(&self, date: NaiveDate) -> anyhow::Result<Vec<FxRate>>;
}
