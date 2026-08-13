use chrono::{DateTime, Utc};

use crate::shared_kernel::{CurrencyCode, UserId};

use super::public::PreferencesError;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct UserPreferences {
    user_id: UserId,
    base_currency: CurrencyCode,
    version: i64,
    persisted: bool,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl UserPreferences {
    pub(crate) fn default_for(user_id: UserId, now: DateTime<Utc>) -> Self {
        Self {
            user_id,
            base_currency: CurrencyCode::new("UAH").expect("UAH is a valid currency code"),
            version: 0,
            persisted: false,
            created_at: now,
            updated_at: now,
        }
    }

    pub(crate) fn reconstitute(
        user_id: UserId,
        base_currency: CurrencyCode,
        version: i64,
        created_at: DateTime<Utc>,
        updated_at: DateTime<Utc>,
    ) -> Result<Self, PreferencesError> {
        if version < 1 {
            return Err(PreferencesError::persistence(
                "stored preference version is invalid",
            ));
        }
        Ok(Self {
            user_id,
            base_currency,
            version,
            persisted: true,
            created_at,
            updated_at,
        })
    }

    pub(crate) fn set_base_currency(
        &mut self,
        base_currency: CurrencyCode,
        expected_version: i64,
        now: DateTime<Utc>,
    ) -> Result<(), PreferencesError> {
        if expected_version != self.version {
            return Err(PreferencesError::version_conflict());
        }
        self.base_currency = base_currency;
        self.version += 1;
        self.persisted = true;
        self.updated_at = now;
        Ok(())
    }

    pub(crate) fn user_id(&self) -> UserId {
        self.user_id
    }

    pub(crate) fn base_currency(&self) -> &CurrencyCode {
        &self.base_currency
    }

    pub(crate) fn version(&self) -> i64 {
        self.version
    }

    pub(crate) fn persisted(&self) -> bool {
        self.persisted
    }

    pub(crate) fn created_at(&self) -> DateTime<Utc> {
        self.created_at
    }

    pub(crate) fn updated_at(&self) -> DateTime<Utc> {
        self.updated_at
    }
}
