//! Stable contracts published by the Reference Data context.

use std::fmt;
use std::future::Future;

use chrono::{DateTime, Utc};

use crate::shared_kernel::CurrencyCode;

use super::application;
pub use super::domain::{ExchangeRate, FxError};
use super::infrastructure::PgCurrencyCatalog;
use super::infrastructure::fx_repository::PgFxRepository;

/// Public Reference Data facade with an unforgeable, privately constructed
/// PostgreSQL adapter.
#[derive(Clone)]
pub struct CurrencyCatalogFacade {
    adapter: PgCurrencyCatalog,
    fx: PgFxRepository,
}

impl CurrencyCatalogFacade {
    pub(crate) fn new(adapter: PgCurrencyCatalog, fx: PgFxRepository) -> Self {
        Self { adapter, fx }
    }
    pub async fn rate_as_of(
        &self,
        base: CurrencyCode,
        quote: CurrencyCode,
        as_of: DateTime<Utc>,
    ) -> Result<FxRateLookup, CurrencyError> {
        application::rate_as_of(&self.fx, base, quote, as_of).await
    }
    pub async fn record_fx_observation(
        &self,
        command: RecordFxObservation,
    ) -> Result<FxObservationResult, CurrencyError> {
        application::record_fx_observation(&self.fx, command).await
    }
}

pub const FX_OBSERVED_V1: &str = "reference-data.fx-observed.v1";
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct FxObservedV1 {
    pub observation_id: uuid::Uuid,
    pub source: String,
    pub source_revision: String,
    pub base_currency: CurrencyCode,
    pub quote_currency: CurrencyCode,
    #[serde(with = "rust_decimal::serde::str")]
    pub rate: rust_decimal::Decimal,
    pub effective_at: DateTime<Utc>,
    pub observed_at: DateTime<Utc>,
    pub recorded_at: DateTime<Utc>,
}
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FxDerivation {
    Direct,
    Inverted,
    Cross { via: CurrencyCode },
}
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct FxRateLookup {
    pub observation_id: uuid::Uuid,
    pub source: String,
    pub source_revision: String,
    pub base_currency: CurrencyCode,
    pub quote_currency: CurrencyCode,
    #[serde(with = "rust_decimal::serde::str")]
    pub rate: rust_decimal::Decimal,
    pub effective_at: DateTime<Utc>,
    pub observed_at: DateTime<Utc>,
    pub recorded_at: DateTime<Utc>,
    pub derivation: FxDerivation,
}

#[derive(Clone, Debug)]
pub struct RecordFxObservation {
    pub source: String,
    pub source_revision: String,
    pub rate: ExchangeRate,
    pub effective_at: DateTime<Utc>,
    pub observed_at: DateTime<Utc>,
    pub recorded_at: DateTime<Utc>,
    pub content_digest: [u8; 32],
}
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct FxObservationResult {
    pub observation_id: uuid::Uuid,
    pub replayed: bool,
}

/// A currency definition safe to share with other bounded contexts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CurrencyDefinition {
    pub code: CurrencyCode,
    pub numeric_code: Option<String>,
    pub name: String,
    pub minor_unit: u8,
    pub enabled: bool,
    pub as_of: DateTime<Utc>,
}

/// Resolves enabled currency definitions without exposing persistence details.
pub trait CurrencyCatalog: Send + Sync {
    fn require_enabled(
        &self,
        code: CurrencyCode,
    ) -> impl Future<Output = Result<CurrencyDefinition, CurrencyError>> + Send;

    fn list_enabled(
        &self,
    ) -> impl Future<Output = Result<Vec<CurrencyDefinition>, CurrencyError>> + Send;
}

pub(crate) type CurrencyView = CurrencyDefinition;

impl CurrencyCatalog for CurrencyCatalogFacade {
    async fn require_enabled(
        &self,
        code: CurrencyCode,
    ) -> Result<CurrencyDefinition, CurrencyError> {
        application::require_enabled(&self.adapter, code).await
    }

    async fn list_enabled(&self) -> Result<Vec<CurrencyDefinition>, CurrencyError> {
        application::list_enabled(&self.adapter).await
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CurrencyErrorKind {
    NotFound,
    Disabled,
    Persistence,
    Conflict,
}

/// Reports a rejected or failed currency-catalog operation.
#[derive(Debug)]
pub struct CurrencyError {
    kind: CurrencyErrorKind,
    message: &'static str,
    cause: Option<Box<dyn std::error::Error + Send + Sync>>,
}

impl CurrencyError {
    pub(crate) fn not_found() -> Self {
        Self {
            kind: CurrencyErrorKind::NotFound,
            message: "currency was not found",
            cause: None,
        }
    }

    pub(crate) fn disabled() -> Self {
        Self {
            kind: CurrencyErrorKind::Disabled,
            message: "currency is disabled",
            cause: None,
        }
    }

    pub(crate) fn database(source: sqlx::Error) -> Self {
        Self::persistence("currency catalog is unavailable").with_source(source)
    }

    pub(crate) fn persistence(message: &'static str) -> Self {
        Self {
            kind: CurrencyErrorKind::Persistence,
            message,
            cause: None,
        }
    }
    pub(crate) fn conflict(message: &'static str) -> Self {
        Self {
            kind: CurrencyErrorKind::Conflict,
            message,
            cause: None,
        }
    }

    fn with_source(mut self, source: impl std::error::Error + Send + Sync + 'static) -> Self {
        self.cause = Some(Box::new(source));
        self
    }

    pub fn is_not_found(&self) -> bool {
        self.kind == CurrencyErrorKind::NotFound
    }

    pub fn is_disabled(&self) -> bool {
        self.kind == CurrencyErrorKind::Disabled
    }
    pub fn is_conflict(&self) -> bool {
        self.kind == CurrencyErrorKind::Conflict
    }
}

impl fmt::Display for CurrencyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for CurrencyError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.cause
            .as_deref()
            .map(|source| source as &(dyn std::error::Error + 'static))
    }
}
