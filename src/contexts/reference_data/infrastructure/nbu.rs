//! NBU anti-corruption adapter and one bounded, fenced synchronization step.

use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use rust_decimal::Decimal;
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row};

use super::super::{
    domain::{ExchangeRate, FxError},
    public::{CurrencyError, RecordFxObservation},
};
use crate::shared_kernel::CurrencyCode;

pub(crate) struct NormalizedNbuRate {
    pub rate: ExchangeRate,
    pub effective_at: DateTime<Utc>,
    pub source_revision: String,
}

pub(crate) fn normalize(
    currency: &str,
    rate: Decimal,
    effective_at: DateTime<Utc>,
) -> Result<NormalizedNbuRate, FxError> {
    let base = CurrencyCode::new(currency).map_err(|_| FxError::CurrencyChain)?;
    let quote = CurrencyCode::new("UAH").map_err(|_| FxError::CurrencyChain)?;
    Ok(NormalizedNbuRate {
        rate: ExchangeRate::new(base.clone(), quote, rate)?,
        effective_at,
        source_revision: format!("{}:{base}", effective_at.date_naive()),
    })
}

#[async_trait]
pub(crate) trait NbuSource: Send + Sync {
    async fn fetch_date(&self, date: NaiveDate) -> anyhow::Result<Vec<NormalizedNbuRate>>;
}

#[derive(Clone)]
pub(crate) struct NbuClient {
    client: reqwest::Client,
    base_url: String,
}

impl NbuClient {
    pub(crate) fn new(base_url: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: base_url.into(),
        }
    }
}

#[async_trait]
impl NbuSource for NbuClient {
    async fn fetch_date(&self, date: NaiveDate) -> anyhow::Result<Vec<NormalizedNbuRate>> {
        #[derive(serde::Deserialize)]
        struct WireRate {
            cc: String,
            rate: serde_json::Value,
            exchangedate: String,
        }
        let wire: Vec<WireRate> = self
            .client
            .get(format!(
                "{}/NBUStatService/v1/statdirectory/exchange",
                self.base_url
            ))
            .query(&[
                ("date", date.format("%Y%m%d").to_string()),
                ("json", String::new()),
            ])
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        wire.into_iter()
            .map(|row| {
                let rate_text = row
                    .rate
                    .as_str()
                    .map(ToOwned::to_owned)
                    .unwrap_or_else(|| row.rate.to_string());
                let rate = rate_text.parse::<Decimal>()?;
                let effective_date = NaiveDate::parse_from_str(&row.exchangedate, "%d.%m.%Y")?;
                let effective_at = Utc
                    .from_local_datetime(
                        &effective_date
                            .and_hms_opt(0, 0, 0)
                            .expect("midnight is valid"),
                    )
                    .single()
                    .expect("UTC has one local representation");
                normalize(&row.cc, rate, effective_at).map_err(anyhow::Error::from)
            })
            .collect()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct FxSyncReport {
    pub claimed: bool,
    pub observations: u32,
    pub replayed: u32,
    pub retry_scheduled: bool,
    pub fenced: bool,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum FxSyncError {
    #[error("FX sync persistence failed")]
    Database(#[from] sqlx::Error),
    #[error("FX sync configuration is invalid")]
    Configuration,
    #[error(transparent)]
    Currency(#[from] CurrencyError),
}

#[derive(Clone)]
pub(crate) struct NbuSyncWorker<S> {
    pool: PgPool,
    source: S,
    holder: String,
    lease_ttl: Duration,
    backfill_days: u32,
}

impl<S> NbuSyncWorker<S>
where
    S: NbuSource,
{
    pub(crate) fn new(
        pool: PgPool,
        source: S,
        holder: impl Into<String>,
        lease_ttl: Duration,
        backfill_days: u32,
    ) -> Result<Self, FxSyncError> {
        let holder = holder.into();
        if holder.trim() != holder
            || holder.is_empty()
            || holder.len() > 200
            || lease_ttl.is_zero()
            || backfill_days > 3650
        {
            return Err(FxSyncError::Configuration);
        }
        Ok(Self {
            pool,
            source,
            holder,
            lease_ttl,
            backfill_days,
        })
    }

    pub(crate) async fn run_once(&self) -> Result<FxSyncReport, FxSyncError> {
        let Some(claim) = self.claim().await? else {
            return Ok(FxSyncReport::default());
        };
        let rates = match self.source.fetch_date(claim.date).await {
            Ok(rates) => rates,
            Err(_) => {
                let retry_scheduled = self.fail(&claim).await?;
                return Ok(FxSyncReport {
                    claimed: true,
                    retry_scheduled,
                    fenced: !retry_scheduled,
                    ..FxSyncReport::default()
                });
            }
        };
        let repository = super::fx_repository::PgFxRepository::new(self.pool.clone());
        let mut inserted = 0_u32;
        let mut replayed = 0_u32;
        for normalized in rates {
            let observed_at = Utc::now();
            let digest: [u8; 32] = Sha256::digest(
                format!(
                    "nbu|{}|{}|{}|{}",
                    normalized.source_revision,
                    normalized.rate.base(),
                    normalized.rate.quote(),
                    normalized.rate.rate()
                )
                .as_bytes(),
            )
            .into();
            let result = repository
                .record_observation(RecordFxObservation {
                    source: "nbu".to_owned(),
                    source_revision: normalized.source_revision,
                    rate: normalized.rate,
                    effective_at: normalized.effective_at,
                    observed_at,
                    recorded_at: observed_at,
                    content_digest: digest,
                })
                .await?;
            if result.replayed {
                replayed += 1;
            } else {
                inserted += 1;
            }
        }
        if !self.complete(&claim).await? {
            return Ok(FxSyncReport {
                claimed: true,
                observations: inserted,
                replayed,
                fenced: true,
                ..FxSyncReport::default()
            });
        }
        Ok(FxSyncReport {
            claimed: true,
            observations: inserted,
            replayed,
            ..FxSyncReport::default()
        })
    }

    async fn claim(&self) -> Result<Option<FxClaim>, FxSyncError> {
        let today = Utc::now().date_naive();
        let initial_date = today - chrono::Duration::days(i64::from(self.backfill_days));
        sqlx::query(
            r#"
            INSERT INTO reference_data.fx_sync_state
                (source,state,date_cursor,backfill_days,fencing_token,attempts,updated_at)
            VALUES('nbu','idle',$1,$2,0,0,clock_timestamp())
            ON CONFLICT(source) DO NOTHING
            "#,
        )
        .bind(initial_date)
        .bind(i32::try_from(self.backfill_days).map_err(|_| FxSyncError::Configuration)?)
        .execute(&self.pool)
        .await?;
        let ttl_millis =
            i64::try_from(self.lease_ttl.as_millis()).map_err(|_| FxSyncError::Configuration)?;
        let row = sqlx::query(
            r#"
            UPDATE reference_data.fx_sync_state SET
                state='running',lease_holder=$1,
                lease_expires_at=clock_timestamp()+($2::bigint*interval '1 millisecond'),
                fencing_token=fencing_token+1,attempts=attempts+1,updated_at=clock_timestamp()
            WHERE source='nbu' AND state IN ('idle','retry_due','running')
              AND date_cursor<=$3
              AND (next_retry_at IS NULL OR next_retry_at<=clock_timestamp())
              AND (lease_expires_at IS NULL OR lease_expires_at<=clock_timestamp())
            RETURNING date_cursor,fencing_token
            "#,
        )
        .bind(&self.holder)
        .bind(ttl_millis)
        .bind(today)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|row| FxClaim {
            date: row.get("date_cursor"),
            token: row.get("fencing_token"),
        }))
    }

    async fn complete(&self, claim: &FxClaim) -> Result<bool, FxSyncError> {
        let updated = sqlx::query(
            r#"
            UPDATE reference_data.fx_sync_state SET state='idle',date_cursor=$4,
                lease_holder=NULL,lease_expires_at=NULL,attempts=0,next_retry_at=NULL,
                last_error=NULL,updated_at=clock_timestamp()
            WHERE source='nbu' AND state='running' AND lease_holder=$1 AND fencing_token=$2
              AND lease_expires_at>clock_timestamp() AND date_cursor=$3
            "#,
        )
        .bind(&self.holder)
        .bind(claim.token)
        .bind(claim.date)
        .bind(claim.date.succ_opt().ok_or(FxSyncError::Configuration)?)
        .execute(&self.pool)
        .await?;
        Ok(updated.rows_affected() == 1)
    }

    async fn fail(&self, claim: &FxClaim) -> Result<bool, FxSyncError> {
        let updated = sqlx::query(
            r#"
            UPDATE reference_data.fx_sync_state SET
                state=CASE WHEN attempts>=10 THEN 'failed' ELSE 'retry_due' END,
                next_retry_at=CASE WHEN attempts>=10 THEN NULL ELSE clock_timestamp()+
                    (LEAST(3600,CAST(power(2,LEAST(attempts,11)) AS BIGINT))*interval '1 second') END,
                last_error='NBU request failed; provider details redacted',
                lease_holder=NULL,lease_expires_at=NULL,updated_at=clock_timestamp()
            WHERE source='nbu' AND state='running' AND lease_holder=$1 AND fencing_token=$2
              AND lease_expires_at>clock_timestamp() AND date_cursor=$3
            "#,
        )
        .bind(&self.holder)
        .bind(claim.token)
        .bind(claim.date)
        .execute(&self.pool)
        .await?;
        Ok(updated.rows_affected() == 1)
    }
}

struct FxClaim {
    date: NaiveDate,
    token: i64,
}
