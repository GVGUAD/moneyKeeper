use async_trait::async_trait;
use chrono::NaiveDate;
use rust_decimal::Decimal;
use serde::Deserialize;

use crate::domain::fx_rate::{FxRate, FxRateSource};

const NBU_URL: &str = "https://bank.gov.ua/NBUStatService/v1/statdirectory/exchange";

pub struct NbuFxRateSource {
    http: reqwest::Client,
    base_url: String,
}

impl Default for NbuFxRateSource {
    fn default() -> Self {
        Self::new()
    }
}

impl NbuFxRateSource {
    pub fn new() -> Self {
        Self {
            http: reqwest::Client::new(),
            base_url: NBU_URL.to_string(),
        }
    }
}

#[derive(Debug, Deserialize)]
struct NbuRow {
    cc: String,
    rate: Decimal,
}

fn parse_rows(date: NaiveDate, rows: Vec<NbuRow>) -> Vec<FxRate> {
    rows.into_iter()
        .map(|row| FxRate {
            rate_date: date,
            from_currency: row.cc,
            to_currency: "UAH".to_string(),
            rate: row.rate,
        })
        .collect()
}

#[async_trait]
impl FxRateSource for NbuFxRateSource {
    async fn fetch_rates_for(&self, date: NaiveDate) -> anyhow::Result<Vec<FxRate>> {
        let date_param = date.format("%Y%m%d").to_string();
        let url = format!("{}?date={}&json", self.base_url, date_param);
        let rows: Vec<NbuRow> = self.http.get(url).send().await?.json().await?;
        Ok(parse_rows(date, rows))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_nbu_rows_to_fx_rates() {
        let date = NaiveDate::from_ymd_opt(2026, 5, 10).unwrap();
        let rows = vec![
            NbuRow { cc: "USD".to_string(), rate: Decimal::new(405, 1) },
            NbuRow { cc: "EUR".to_string(), rate: Decimal::new(432, 1) },
        ];

        let rates = parse_rows(date, rows);

        assert_eq!(rates.len(), 2);
        assert_eq!(rates[0].from_currency, "USD");
        assert_eq!(rates[0].to_currency, "UAH");
        assert_eq!(rates[0].rate_date, date);
        assert_eq!(rates[0].rate, Decimal::new(405, 1));
    }
}
