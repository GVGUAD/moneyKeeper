use async_trait::async_trait;
use chrono::NaiveDate;
use rust_decimal::Decimal;
use sqlx::PgPool;

use crate::domain::fx_rate::{FxQuote, FxRate, FxRateRepository};

pub struct PgFxRateRepository {
    pool: PgPool,
}

impl PgFxRateRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl FxRateRepository for PgFxRateRepository {
    async fn rate_as_of(
        &self,
        date: NaiveDate,
        from: &str,
        to: &str,
    ) -> anyhow::Result<Option<Decimal>> {
        let from = from.to_uppercase();
        let to = to.to_uppercase();
        if from == to {
            return Ok(Some(Decimal::ONE));
        }
        let from_to_uah = if from == "UAH" {
            Some(Decimal::ONE)
        } else {
            sqlx::query_scalar!(
                r#"
                SELECT rate FROM fx_rates
                WHERE from_currency = $1 AND to_currency = 'UAH' AND rate_date <= $2
                ORDER BY rate_date DESC
                LIMIT 1
                "#,
                from,
                date,
            )
            .fetch_optional(&self.pool)
            .await?
        };
        let to_to_uah = if to == "UAH" {
            Some(Decimal::ONE)
        } else {
            sqlx::query_scalar!(
                r#"
                SELECT rate FROM fx_rates
                WHERE from_currency = $1 AND to_currency = 'UAH' AND rate_date <= $2
                ORDER BY rate_date DESC
                LIMIT 1
                "#,
                to,
                date,
            )
            .fetch_optional(&self.pool)
            .await?
        };
        match (from_to_uah, to_to_uah) {
            (Some(f), Some(t)) if !t.is_zero() => Ok(Some(f / t)),
            _ => Ok(None),
        }
    }

    async fn quote_as_of(
        &self,
        date: NaiveDate,
        from: &str,
        to: &str,
    ) -> anyhow::Result<Option<FxQuote>> {
        let from = from.to_ascii_uppercase();
        let to = to.to_ascii_uppercase();
        if from == to {
            return Ok(Some(FxQuote {
                requested_date: date,
                rate_date: date,
                from_currency: from,
                to_currency: to,
                rate: Decimal::ONE,
            }));
        }
        let from_quote = if from == "UAH" {
            Some((date, Decimal::ONE))
        } else {
            sqlx::query_as::<_, (NaiveDate, Decimal)>(
                "SELECT rate_date, rate FROM fx_rates \
                 WHERE from_currency=$1 AND to_currency='UAH' AND rate_date <= $2 \
                 ORDER BY rate_date DESC LIMIT 1",
            )
            .bind(&from)
            .bind(date)
            .fetch_optional(&self.pool)
            .await?
        };
        let to_quote = if to == "UAH" {
            Some((date, Decimal::ONE))
        } else {
            sqlx::query_as::<_, (NaiveDate, Decimal)>(
                "SELECT rate_date, rate FROM fx_rates \
                 WHERE from_currency=$1 AND to_currency='UAH' AND rate_date <= $2 \
                 ORDER BY rate_date DESC LIMIT 1",
            )
            .bind(&to)
            .bind(date)
            .fetch_optional(&self.pool)
            .await?
        };
        let (Some((from_date, from_rate)), Some((to_date, to_rate))) = (from_quote, to_quote)
        else {
            return Ok(None);
        };
        if to_rate.is_zero() {
            return Ok(None);
        }
        Ok(Some(FxQuote {
            requested_date: date,
            rate_date: from_date.min(to_date),
            from_currency: from,
            to_currency: to,
            rate: from_rate / to_rate,
        }))
    }

    async fn upsert_many(&self, rates: &[FxRate]) -> anyhow::Result<()> {
        if rates.is_empty() {
            return Ok(());
        }
        let mut tx = self.pool.begin().await?;
        for r in rates {
            let from = r.from_currency.to_uppercase();
            let to = r.to_currency.to_uppercase();
            sqlx::query!(
                r#"
                INSERT INTO fx_rates (rate_date, from_currency, to_currency, rate)
                VALUES ($1, $2, $3, $4)
                ON CONFLICT (rate_date, from_currency, to_currency)
                DO UPDATE SET rate = EXCLUDED.rate
                "#,
                r.rate_date,
                from,
                to,
                r.rate,
            )
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    async fn latest_date(&self) -> anyhow::Result<Option<NaiveDate>> {
        Ok(sqlx::query_scalar!("SELECT MAX(rate_date) FROM fx_rates")
            .fetch_one(&self.pool)
            .await?)
    }

    async fn missing_dates(
        &self,
        from: NaiveDate,
        to: NaiveDate,
    ) -> anyhow::Result<Vec<NaiveDate>> {
        if from > to {
            return Ok(Vec::new());
        }
        Ok(sqlx::query_scalar::<_, NaiveDate>(
            "SELECT day::date \
             FROM generate_series($1::date,$2::date,interval '1 day') AS day \
             WHERE NOT EXISTS (SELECT 1 FROM fx_rates WHERE rate_date=day::date) \
             ORDER BY day",
        )
        .bind(from)
        .bind(to)
        .fetch_all(&self.pool)
        .await?)
    }

    async fn known_currencies(&self) -> anyhow::Result<Vec<String>> {
        let rows = sqlx::query_scalar!(
            "SELECT DISTINCT from_currency FROM fx_rates WHERE from_currency IS NOT NULL ORDER BY from_currency"
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }
}
