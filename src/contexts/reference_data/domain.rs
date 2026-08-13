use chrono::{DateTime, Utc};

use crate::shared_kernel::CurrencyCode;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CurrencyDefinition {
    pub(crate) code: CurrencyCode,
    pub(crate) numeric_code: Option<String>,
    pub(crate) name: String,
    pub(crate) minor_unit: u8,
    pub(crate) enabled: bool,
    pub(crate) updated_at: DateTime<Utc>,
}
