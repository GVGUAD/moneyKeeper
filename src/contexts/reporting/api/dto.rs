use crate::contexts::reporting::public::ReportRange;
use crate::shared_kernel::CurrencyCode;
use chrono::{DateTime, Utc};
use serde::Deserialize;
#[derive(Deserialize)]
pub(crate) struct ReportQuery {
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
    pub timezone: String,
    pub base_currency: Option<String>,
}
impl TryFrom<ReportQuery> for ReportRange {
    type Error = ();
    fn try_from(v: ReportQuery) -> Result<Self, Self::Error> {
        if v.from >= v.to || v.timezone.trim().is_empty() {
            return Err(());
        }
        Ok(Self {
            from: v.from,
            to: v.to,
            timezone: v.timezone,
            base_currency: v
                .base_currency
                .map(CurrencyCode::new)
                .transpose()
                .map_err(|_| ())?,
        })
    }
}
