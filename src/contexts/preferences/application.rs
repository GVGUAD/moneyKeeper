use chrono::{DateTime, Utc};

use crate::contexts::reference_data::public::CurrencyCatalog;
use crate::shared_kernel::{CurrencyCode, UserId};

use super::domain::UserPreferences;
use super::infrastructure::PgPreferences;
use super::public::{PreferencesError, PreferencesView};

pub(crate) async fn get(
    preferences: &PgPreferences,
    user_id: UserId,
    now: DateTime<Utc>,
) -> Result<PreferencesView, PreferencesError> {
    Ok(preferences
        .find(user_id)
        .await?
        .unwrap_or_else(|| UserPreferences::default_for(user_id, now))
        .into())
}

pub(crate) async fn set_base_currency<C: CurrencyCatalog>(
    preferences: &PgPreferences,
    currencies: &C,
    user_id: UserId,
    base_currency: CurrencyCode,
    expected_version: i64,
    now: DateTime<Utc>,
) -> Result<PreferencesView, PreferencesError> {
    currencies
        .require_enabled(base_currency.clone())
        .await
        .map_err(PreferencesError::currency)?;
    let mut value = preferences
        .find(user_id)
        .await?
        .unwrap_or_else(|| UserPreferences::default_for(user_id, now));
    value.set_base_currency(base_currency, expected_version, now)?;
    preferences.save(&value, expected_version).await?;
    Ok(value.into())
}

impl From<UserPreferences> for PreferencesView {
    fn from(preferences: UserPreferences) -> Self {
        Self {
            user_id: preferences.user_id(),
            base_currency: preferences.base_currency().clone(),
            version: preferences.version(),
            persisted: preferences.persisted(),
            as_of: preferences.updated_at(),
        }
    }
}
