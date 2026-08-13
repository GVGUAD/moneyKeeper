use chrono::{DateTime, Utc};
use sqlx::PgPool;

use crate::shared_kernel::CurrencyCode;

use super::domain::CurrencyDefinition;
use super::public::CurrencyError;

#[derive(sqlx::FromRow)]
struct CurrencyRow {
    code: String,
    numeric_code: Option<String>,
    name: String,
    minor_unit: i16,
    enabled: bool,
    updated_at: DateTime<Utc>,
}

impl CurrencyRow {
    fn into_domain(self) -> Result<CurrencyDefinition, CurrencyError> {
        let code = CurrencyCode::new(self.code)
            .map_err(|_| CurrencyError::persistence("stored currency code is invalid"))?;
        let minor_unit = u8::try_from(self.minor_unit)
            .map_err(|_| CurrencyError::persistence("stored currency scale is invalid"))?;
        Ok(CurrencyDefinition {
            code,
            numeric_code: self.numeric_code,
            name: self.name,
            minor_unit,
            enabled: self.enabled,
            updated_at: self.updated_at,
        })
    }
}

/// PostgreSQL-backed ISO currency catalog.
#[derive(Clone)]
pub(crate) struct PgCurrencyCatalog {
    pool: PgPool,
}

impl PgCurrencyCatalog {
    /// Creates a catalog backed by a verified Finance V2 database pool.
    pub(crate) fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub(crate) async fn find(
        &self,
        code: CurrencyCode,
    ) -> Result<Option<CurrencyDefinition>, CurrencyError> {
        let row = sqlx::query_as::<_, CurrencyRow>(
            "SELECT code::text AS code, numeric_code::text AS numeric_code, name, \
                    minor_unit, enabled, updated_at \
             FROM reference_data.currencies WHERE code = $1",
        )
        .bind(code.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(CurrencyError::database)?;
        row.map(CurrencyRow::into_domain).transpose()
    }

    pub(crate) async fn list_enabled_definitions(
        &self,
    ) -> Result<Vec<CurrencyDefinition>, CurrencyError> {
        sqlx::query_as::<_, CurrencyRow>(
            "SELECT code::text AS code, numeric_code::text AS numeric_code, name, \
                    minor_unit, enabled, updated_at \
             FROM reference_data.currencies WHERE enabled ORDER BY code",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(CurrencyError::database)?
        .into_iter()
        .map(CurrencyRow::into_domain)
        .collect()
    }
}
