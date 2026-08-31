use crate::shared_kernel::CurrencyCode;
use async_trait::async_trait;

use super::domain::CurrencyDefinition;
use super::public::{CurrencyError, CurrencyView};
use super::public::{FxObservationResult, FxRateLookup, RecordFxObservation};
use chrono::{DateTime, Utc};

#[async_trait]
pub(crate) trait CurrencyRepository: Send + Sync {
    async fn find(&self, code: CurrencyCode) -> Result<Option<CurrencyDefinition>, CurrencyError>;
    async fn list_enabled_definitions(&self) -> Result<Vec<CurrencyDefinition>, CurrencyError>;
    async fn list_known_definitions(&self) -> Result<Vec<CurrencyDefinition>, CurrencyError>;
}

#[async_trait]
pub(crate) trait FxObservationRepository: Send + Sync {
    async fn rate_as_of(
        &self,
        base: CurrencyCode,
        quote: CurrencyCode,
        as_of: DateTime<Utc>,
    ) -> Result<FxRateLookup, CurrencyError>;
    async fn record_observation(
        &self,
        command: RecordFxObservation,
    ) -> Result<FxObservationResult, CurrencyError>;
}

pub(crate) async fn require_enabled<R: CurrencyRepository + ?Sized>(
    catalog: &R,
    code: CurrencyCode,
) -> Result<CurrencyView, CurrencyError> {
    match catalog.find(code).await? {
        Some(definition) if definition.enabled => Ok(definition.into()),
        Some(_) => Err(CurrencyError::disabled()),
        None => Err(CurrencyError::not_found()),
    }
}

pub(crate) async fn list_enabled<R: CurrencyRepository + ?Sized>(
    catalog: &R,
) -> Result<Vec<CurrencyView>, CurrencyError> {
    catalog
        .list_enabled_definitions()
        .await
        .map(|definitions| definitions.into_iter().map(Into::into).collect())
}

pub(crate) async fn list_known<R: CurrencyRepository + ?Sized>(
    catalog: &R,
) -> Result<Vec<CurrencyView>, CurrencyError> {
    catalog
        .list_known_definitions()
        .await
        .map(|definitions| definitions.into_iter().map(Into::into).collect())
}

pub(crate) async fn rate_as_of<R: FxObservationRepository + ?Sized>(
    repository: &R,
    base: CurrencyCode,
    quote: CurrencyCode,
    as_of: DateTime<Utc>,
) -> Result<FxRateLookup, CurrencyError> {
    if base == quote {
        return Err(CurrencyError::invalid(
            "base and quote currencies must differ",
        ));
    }
    repository.rate_as_of(base, quote, as_of).await
}

pub(crate) async fn record_fx_observation<R: FxObservationRepository + ?Sized>(
    repository: &R,
    command: RecordFxObservation,
) -> Result<FxObservationResult, CurrencyError> {
    if command.source.trim() != command.source
        || command.source.is_empty()
        || command.source_revision.trim() != command.source_revision
        || command.source_revision.is_empty()
    {
        return Err(CurrencyError::invalid("invalid FX source identity"));
    }
    if command.effective_at > command.observed_at || command.observed_at > command.recorded_at {
        return Err(CurrencyError::invalid("invalid FX observation time order"));
    }
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
