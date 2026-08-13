use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use crate::shared_kernel::{CurrencyCode, UserId};

use super::domain::UserPreferences;
use super::public::PreferencesError;

#[derive(sqlx::FromRow)]
struct PreferencesRow {
    user_id: Uuid,
    base_currency: String,
    version: i64,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl PreferencesRow {
    fn into_domain(self) -> Result<UserPreferences, PreferencesError> {
        let base_currency = CurrencyCode::new(self.base_currency)
            .map_err(|_| PreferencesError::persistence("stored base currency is invalid"))?;
        UserPreferences::reconstitute(
            UserId::new(self.user_id),
            base_currency,
            self.version,
            self.created_at,
            self.updated_at,
        )
    }
}

/// PostgreSQL-backed user preferences capability.
#[derive(Clone)]
pub(crate) struct PgPreferences {
    pool: PgPool,
}

impl PgPreferences {
    /// Creates a preferences capability backed by a Finance V2 pool.
    pub(crate) fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub(crate) async fn find(
        &self,
        user_id: UserId,
    ) -> Result<Option<UserPreferences>, PreferencesError> {
        sqlx::query_as::<_, PreferencesRow>(
            "SELECT user_id, base_currency::text AS base_currency, version, \
                    created_at, updated_at \
             FROM preferences.user_preferences WHERE user_id = $1",
        )
        .bind(user_id.into_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(PreferencesError::database)?
        .map(PreferencesRow::into_domain)
        .transpose()
    }

    pub(crate) async fn save(
        &self,
        preferences: &UserPreferences,
        expected_version: i64,
    ) -> Result<(), PreferencesError> {
        let result = if expected_version == 0 {
            sqlx::query(
                "INSERT INTO preferences.user_preferences \
                 (user_id, base_currency, version, created_at, updated_at) \
                 VALUES ($1, $2, $3, $4, $5) ON CONFLICT (user_id) DO NOTHING",
            )
            .bind(preferences.user_id().into_uuid())
            .bind(preferences.base_currency().as_str())
            .bind(preferences.version())
            .bind(preferences.created_at())
            .bind(preferences.updated_at())
            .execute(&self.pool)
            .await
            .map_err(PreferencesError::database)?
        } else {
            sqlx::query(
                "UPDATE preferences.user_preferences \
                 SET base_currency = $1, version = $2, updated_at = $3 \
                 WHERE user_id = $4 AND version = $5",
            )
            .bind(preferences.base_currency().as_str())
            .bind(preferences.version())
            .bind(preferences.updated_at())
            .bind(preferences.user_id().into_uuid())
            .bind(expected_version)
            .execute(&self.pool)
            .await
            .map_err(PreferencesError::database)?
        };
        if result.rows_affected() == 0 {
            return Err(PreferencesError::version_conflict());
        }
        Ok(())
    }
}
