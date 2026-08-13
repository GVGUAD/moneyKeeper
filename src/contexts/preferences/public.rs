//! Stable contracts published by the Preferences context.

use std::fmt;
use std::future::Future;

use chrono::{DateTime, Utc};

use crate::contexts::reference_data::public::{CurrencyCatalog, CurrencyError};
use crate::shared_kernel::{CurrencyCode, UserId};

use super::application;
use super::infrastructure::PgPreferences;

/// Public Preferences facade with privately assembled persistence.
#[derive(Clone)]
pub struct PreferencesFacade {
    adapter: PgPreferences,
}

impl PreferencesFacade {
    pub(crate) fn new(adapter: PgPreferences) -> Self {
        Self { adapter }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreferencesView {
    pub user_id: UserId,
    pub base_currency: CurrencyCode,
    pub version: i64,
    pub persisted: bool,
    pub as_of: DateTime<Utc>,
}

pub trait Preferences: Send + Sync {
    fn get(
        &self,
        user_id: UserId,
        now: DateTime<Utc>,
    ) -> impl Future<Output = Result<PreferencesView, PreferencesError>> + Send;

    fn set_base_currency<C: CurrencyCatalog>(
        &self,
        currencies: &C,
        user_id: UserId,
        base_currency: CurrencyCode,
        expected_version: i64,
        now: DateTime<Utc>,
    ) -> impl Future<Output = Result<PreferencesView, PreferencesError>> + Send;
}

impl Preferences for PreferencesFacade {
    async fn get(
        &self,
        user_id: UserId,
        now: DateTime<Utc>,
    ) -> Result<PreferencesView, PreferencesError> {
        application::get(&self.adapter, user_id, now).await
    }

    async fn set_base_currency<C: CurrencyCatalog>(
        &self,
        currencies: &C,
        user_id: UserId,
        base_currency: CurrencyCode,
        expected_version: i64,
        now: DateTime<Utc>,
    ) -> Result<PreferencesView, PreferencesError> {
        application::set_base_currency(
            &self.adapter,
            currencies,
            user_id,
            base_currency,
            expected_version,
            now,
        )
        .await
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PreferencesErrorKind {
    Currency,
    VersionConflict,
    Persistence,
}

#[derive(Debug)]
pub struct PreferencesError {
    kind: PreferencesErrorKind,
    message: &'static str,
    cause: Option<Box<dyn std::error::Error + Send + Sync>>,
}

impl PreferencesError {
    pub(crate) fn currency(source: CurrencyError) -> Self {
        Self::new(
            PreferencesErrorKind::Currency,
            "base currency is not enabled",
        )
        .with_source(source)
    }

    pub(crate) fn version_conflict() -> Self {
        Self::new(
            PreferencesErrorKind::VersionConflict,
            "preferences version conflict",
        )
    }

    pub(crate) fn persistence(message: &'static str) -> Self {
        Self::new(PreferencesErrorKind::Persistence, message)
    }

    pub(crate) fn database(source: sqlx::Error) -> Self {
        Self::persistence("preferences storage is unavailable").with_source(source)
    }

    fn new(kind: PreferencesErrorKind, message: &'static str) -> Self {
        Self {
            kind,
            message,
            cause: None,
        }
    }

    fn with_source(mut self, source: impl std::error::Error + Send + Sync + 'static) -> Self {
        self.cause = Some(Box::new(source));
        self
    }

    pub fn is_currency_rejected(&self) -> bool {
        self.kind == PreferencesErrorKind::Currency
    }

    pub fn is_version_conflict(&self) -> bool {
        self.kind == PreferencesErrorKind::VersionConflict
    }
}

impl fmt::Display for PreferencesError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for PreferencesError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.cause
            .as_deref()
            .map(|source| source as &(dyn std::error::Error + 'static))
    }
}
