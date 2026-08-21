use super::super::public::{
    CurrencyError, FxDerivation, FxObservationResult, FxRateLookup, RecordFxObservation,
};
use crate::shared_kernel::CurrencyCode;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use sqlx::{PgPool, Row};
#[derive(Clone)]
pub(crate) struct PgFxRepository {
    pool: PgPool,
}
impl PgFxRepository {
    pub(crate) fn new(pool: PgPool) -> Self {
        Self { pool }
    }
    pub(crate) async fn rate_as_of(
        &self,
        base: CurrencyCode,
        quote: CurrencyCode,
        as_of: DateTime<Utc>,
    ) -> Result<FxRateLookup, CurrencyError> {
        if base == quote {
            return Err(CurrencyError::persistence(
                "base and quote currencies must differ",
            ));
        }
        let direct=sqlx::query("SELECT id,source,source_revision,base_currency::text,quote_currency::text,rate,effective_at,observed_at,recorded_at FROM reference_data.fx_observations WHERE base_currency=$1 AND quote_currency=$2 AND effective_at<=$3 ORDER BY effective_at DESC,source_priority,observed_at DESC,sequence DESC,id DESC LIMIT 1").bind(base.as_str()).bind(quote.as_str()).bind(as_of).fetch_optional(&self.pool).await.map_err(CurrencyError::database)?;
        if let Some(r) = direct {
            return row_lookup(r, FxDerivation::Direct);
        }
        let inverse=sqlx::query("SELECT id,source,source_revision,base_currency::text,quote_currency::text,rate,effective_at,observed_at,recorded_at FROM reference_data.fx_observations WHERE base_currency=$2 AND quote_currency=$1 AND effective_at<=$3 ORDER BY effective_at DESC,source_priority,observed_at DESC,sequence DESC,id DESC LIMIT 1").bind(base.as_str()).bind(quote.as_str()).bind(as_of).fetch_optional(&self.pool).await.map_err(CurrencyError::database)?;
        let Some(r) = inverse else {
            let pivot = CurrencyCode::new("UAH")
                .map_err(|_| CurrencyError::persistence("FX pivot currency is invalid"))?;
            if base != pivot && quote != pivot {
                let base_leg = latest_direct(&self.pool, &base, &pivot, as_of).await?;
                let quote_leg = latest_direct(&self.pool, &quote, &pivot, as_of).await?;
                if let (Some(base_leg), Some(quote_leg)) = (base_leg, quote_leg) {
                    let base_lookup = row_lookup(base_leg, FxDerivation::Direct)?;
                    let quote_lookup = row_lookup(quote_leg, FxDerivation::Direct)?;
                    let rate = base_lookup
                        .rate
                        .checked_div(quote_lookup.rate)
                        .ok_or_else(|| CurrencyError::persistence("FX cross rate overflowed"))?;
                    return Ok(FxRateLookup {
                        observation_id: base_lookup.observation_id,
                        source: format!("{}+{}", base_lookup.source, quote_lookup.source),
                        source_revision: format!(
                            "{}+{}",
                            base_lookup.source_revision, quote_lookup.source_revision
                        ),
                        base_currency: base,
                        quote_currency: quote,
                        rate,
                        effective_at: base_lookup.effective_at.min(quote_lookup.effective_at),
                        observed_at: base_lookup.observed_at.max(quote_lookup.observed_at),
                        recorded_at: base_lookup.recorded_at.max(quote_lookup.recorded_at),
                        derivation: FxDerivation::Cross { via: pivot },
                    });
                }
            }
            return Err(CurrencyError::not_found());
        };
        let mut value = row_lookup(r, FxDerivation::Inverted)?;
        value.base_currency = base;
        value.quote_currency = quote;
        value.rate = Decimal::ONE
            .checked_div(value.rate)
            .ok_or_else(|| CurrencyError::persistence("stored FX rate cannot be inverted"))?;
        Ok(value)
    }

    pub(crate) async fn record_observation(
        &self,
        command: RecordFxObservation,
    ) -> Result<FxObservationResult, CurrencyError> {
        if command.source.trim() != command.source
            || command.source.is_empty()
            || command.source_revision.trim() != command.source_revision
            || command.source_revision.is_empty()
        {
            return Err(CurrencyError::persistence("invalid FX source identity"));
        }
        if command.effective_at > command.observed_at || command.observed_at > command.recorded_at {
            return Err(CurrencyError::persistence(
                "invalid FX observation time order",
            ));
        }
        let mut tx = self.pool.begin().await.map_err(CurrencyError::database)?;
        let id = uuid::Uuid::new_v4();
        let inserted=sqlx::query("INSERT INTO reference_data.fx_observations(id,source,source_revision,base_currency,quote_currency,rate,effective_at,observed_at,recorded_at,content_digest) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10) ON CONFLICT(source,source_revision,base_currency,quote_currency) DO NOTHING").bind(id).bind(&command.source).bind(&command.source_revision).bind(command.rate.base().as_str()).bind(command.rate.quote().as_str()).bind(command.rate.rate()).bind(command.effective_at).bind(command.observed_at).bind(command.recorded_at).bind(command.content_digest.as_slice()).execute(&mut *tx).await.map_err(CurrencyError::database)?;
        if inserted.rows_affected() == 0 {
            let row=sqlx::query("SELECT id,content_digest FROM reference_data.fx_observations WHERE source=$1 AND source_revision=$2 AND base_currency=$3 AND quote_currency=$4").bind(&command.source).bind(&command.source_revision).bind(command.rate.base().as_str()).bind(command.rate.quote().as_str()).fetch_one(&mut *tx).await.map_err(CurrencyError::database)?;
            if row.get::<Vec<u8>, _>("content_digest") != command.content_digest {
                sqlx::query("INSERT INTO reference_data.fx_conflicts(id,source,source_revision,conflicting_digest,reason,recorded_at) VALUES($1,$2,$3,$4,'same source revision has different content',$5) ON CONFLICT DO NOTHING").bind(uuid::Uuid::new_v4()).bind(&command.source).bind(&command.source_revision).bind(command.content_digest.as_slice()).bind(command.recorded_at).execute(&mut *tx).await.map_err(CurrencyError::database)?;
                tx.commit().await.map_err(CurrencyError::database)?;
                return Err(CurrencyError::conflict(
                    "FX source revision conflicts with recorded content",
                ));
            }
            let existing = row.get("id");
            tx.commit().await.map_err(CurrencyError::database)?;
            return Ok(FxObservationResult {
                observation_id: existing,
                replayed: true,
            });
        }
        let payload = serde_json::json!({"observation_id":id,"source":command.source,"source_revision":command.source_revision,"base_currency":command.rate.base(),"quote_currency":command.rate.quote(),"rate":command.rate.rate().to_string(),"effective_at":command.effective_at,"observed_at":command.observed_at,"recorded_at":command.recorded_at});
        sqlx::query("INSERT INTO integration.outbox_messages(message_id,event_id,message_schema_version,context_name,aggregate_id,aggregate_version,event_type,user_id,occurred_at,correlation_id,payload) VALUES($1,$2,1,'reference-data',$3,1,'reference-data.fx-observed.v1',$4,$5,$6,$7)").bind(uuid::Uuid::new_v4()).bind(uuid::Uuid::new_v4()).bind(id.to_string()).bind(uuid::Uuid::nil()).bind(command.recorded_at).bind(id).bind(payload).execute(&mut *tx).await.map_err(CurrencyError::database)?;
        tx.commit().await.map_err(CurrencyError::database)?;
        Ok(FxObservationResult {
            observation_id: id,
            replayed: false,
        })
    }
}

async fn latest_direct(
    pool: &PgPool,
    base: &CurrencyCode,
    quote: &CurrencyCode,
    as_of: DateTime<Utc>,
) -> Result<Option<sqlx::postgres::PgRow>, CurrencyError> {
    sqlx::query("SELECT id,source,source_revision,base_currency::text,quote_currency::text,rate,effective_at,observed_at,recorded_at FROM reference_data.fx_observations WHERE base_currency=$1 AND quote_currency=$2 AND effective_at<=$3 ORDER BY effective_at DESC,source_priority,observed_at DESC,sequence DESC,id DESC LIMIT 1")
        .bind(base.as_str()).bind(quote.as_str()).bind(as_of).fetch_optional(pool).await.map_err(CurrencyError::database)
}
fn row_lookup(
    r: sqlx::postgres::PgRow,
    derivation: FxDerivation,
) -> Result<FxRateLookup, CurrencyError> {
    Ok(FxRateLookup {
        observation_id: r.get("id"),
        source: r.get("source"),
        source_revision: r.get("source_revision"),
        base_currency: CurrencyCode::new(r.get::<String, _>("base_currency"))
            .map_err(|_| CurrencyError::persistence("stored FX currency is invalid"))?,
        quote_currency: CurrencyCode::new(r.get::<String, _>("quote_currency"))
            .map_err(|_| CurrencyError::persistence("stored FX currency is invalid"))?,
        rate: r.get("rate"),
        effective_at: r.get("effective_at"),
        observed_at: r.get("observed_at"),
        recorded_at: r.get("recorded_at"),
        derivation,
    })
}
