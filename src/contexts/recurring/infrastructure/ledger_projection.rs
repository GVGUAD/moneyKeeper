//! Local, replayable projection of public Ledger candidates.
use crate::shared_kernel::{CurrencyCode, UserId};
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LedgerCandidate {
    pub journal_entry_id: uuid::Uuid,
    pub user_id: UserId,
    pub amount: Decimal,
    pub currency: CurrencyCode,
    pub merchant: Option<String>,
    pub category_id: Option<uuid::Uuid>,
    pub reversed: bool,
    pub occurred_at: DateTime<Utc>,
    pub source_sequence: u64,
}
pub(crate) fn score(merchant: &str, candidate: &LedgerCandidate) -> u16 {
    let needle = merchant.trim().to_ascii_lowercase();
    candidate
        .merchant
        .as_ref()
        .map(|m| m.to_ascii_lowercase())
        .map_or(0, |m| {
            if m == needle {
                100
            } else if m.contains(&needle) || needle.contains(&m) {
                70
            } else {
                0
            }
        })
}
