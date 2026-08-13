use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::contexts::preferences::public::PreferencesView;
use crate::shared_kernel::CurrencyCode;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct UpdatePreferencesRequest {
    pub(crate) base_currency: String,
    pub(crate) expected_version: i64,
}

#[derive(Debug, Serialize)]
pub(crate) struct PreferencesResponse {
    base_currency: CurrencyCode,
    version: i64,
    persisted: bool,
    as_of: DateTime<Utc>,
}

impl From<PreferencesView> for PreferencesResponse {
    fn from(value: PreferencesView) -> Self {
        Self {
            base_currency: value.base_currency,
            version: value.version,
            persisted: value.persisted,
            as_of: value.as_of,
        }
    }
}
