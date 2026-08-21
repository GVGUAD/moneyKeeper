use crate::shared_kernel::CurrencyCode;

use super::domain::CurrencyDefinition;
use super::infrastructure::PgCurrencyCatalog;
use super::infrastructure::fx_repository::PgFxRepository;
use super::public::{CurrencyError, CurrencyView};
use super::public::{FxObservationResult, FxRateLookup, RecordFxObservation};
use chrono::{DateTime, Utc};

pub(crate) async fn require_enabled(
    catalog: &PgCurrencyCatalog,
    code: CurrencyCode,
) -> Result<CurrencyView, CurrencyError> {
    match catalog.find(code).await? {
        Some(definition) if definition.enabled => Ok(definition.into()),
        Some(_) => Err(CurrencyError::disabled()),
        None => Err(CurrencyError::not_found()),
    }
}

pub(crate) async fn list_enabled(
    catalog: &PgCurrencyCatalog,
) -> Result<Vec<CurrencyView>, CurrencyError> {
    catalog
        .list_enabled_definitions()
        .await
        .map(|definitions| definitions.into_iter().map(Into::into).collect())
}

pub(crate) async fn rate_as_of(
    repository: &PgFxRepository,
    base: CurrencyCode,
    quote: CurrencyCode,
    as_of: DateTime<Utc>,
) -> Result<FxRateLookup, CurrencyError> {
    repository.rate_as_of(base, quote, as_of).await
}

pub(crate) async fn record_fx_observation(
    repository: &PgFxRepository,
    command: RecordFxObservation,
) -> Result<FxObservationResult, CurrencyError> {
    repository.record_observation(command).await
}

impl From<CurrencyDefinition> for CurrencyView {
    fn from(definition: CurrencyDefinition) -> Self {
        Self {
            code: definition.code,
            numeric_code: definition.numeric_code,
            name: definition.name,
            minor_unit: definition.minor_unit,
            enabled: definition.enabled,
            as_of: definition.updated_at,
        }
    }
}
