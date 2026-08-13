//! Stable contracts published by the Reference Data context.

use std::fmt;
use std::future::Future;

use chrono::{DateTime, Utc};

use crate::shared_kernel::CurrencyCode;

use super::application;
use super::infrastructure::PgCurrencyCatalog;

/// Public Reference Data facade with an unforgeable, privately constructed
/// PostgreSQL adapter.
#[derive(Clone)]
pub struct CurrencyCatalogFacade {
    adapter: PgCurrencyCatalog,
}

impl CurrencyCatalogFacade {
    pub(crate) fn new(adapter: PgCurrencyCatalog) -> Self {
        Self { adapter }
    }
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
