use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::contexts::reference_data::public::CurrencyDefinition;
use crate::shared_kernel::CurrencyCode;

#[derive(Debug, Serialize)]
pub(crate) struct CurrencyResponse {
    code: CurrencyCode,
    numeric_code: Option<String>,
    name: String,
    minor_unit: u8,
    enabled: bool,
    as_of: DateTime<Utc>,
}

impl From<CurrencyDefinition> for CurrencyResponse {
    fn from(value: CurrencyDefinition) -> Self {
        Self {
            code: value.code,
            numeric_code: value.numeric_code,
            name: value.name,
            minor_unit: value.minor_unit,
            enabled: value.enabled,
            as_of: value.as_of,
        }
    }
}
