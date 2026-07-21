use chrono::NaiveDate;
use rust_decimal::Decimal;

#[derive(Debug, Clone, PartialEq)]
pub struct FxRate {
    pub rate_date: NaiveDate,
    pub from_currency: String,
    pub to_currency: String,
    pub rate: Decimal,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FxQuote {
    pub requested_date: NaiveDate,
    /// Actual rate date used. For a cross-rate this is the older of the two
    /// component quote dates.
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

    async fn quote_as_of(
        &self,
        date: NaiveDate,
        from: &str,
        to: &str,
    ) -> anyhow::Result<Option<FxQuote>> {
        Ok(self.rate_as_of(date, from, to).await?.map(|rate| FxQuote {
            requested_date: date,
            rate_date: date,
            from_currency: from.to_ascii_uppercase(),
            to_currency: to.to_ascii_uppercase(),
            rate,
        }))
    }

    async fn upsert_many(&self, rates: &[FxRate]) -> anyhow::Result<()>;

    async fn latest_date(&self) -> anyhow::Result<Option<NaiveDate>>;

    /// Dates in the inclusive range that have no stored market quotes. The
    /// default keeps in-memory/test repositories simple; persistent
    /// repositories should override it to avoid redundant provider calls.
    async fn missing_dates(
        &self,
        from: NaiveDate,
        to: NaiveDate,
    ) -> anyhow::Result<Vec<NaiveDate>> {
        let mut dates = Vec::new();
        let mut date = from;
        while date <= to {
            dates.push(date);
            date += chrono::Duration::days(1);
        }
        Ok(dates)
    }

    /// Distinct `from_currency` values present in the table.
    async fn known_currencies(&self) -> anyhow::Result<Vec<String>>;
}

#[async_trait::async_trait]
pub trait FxRateSource: Send + Sync {
    /// Fetch all available rates against UAH for `date`.
    async fn fetch_rates_for(&self, date: NaiveDate) -> anyhow::Result<Vec<FxRate>>;
}
