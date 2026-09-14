//! Exact live accounting read contracts. Category sets are explicit, including empty sets.
use super::public::{ActivityCursor, ActivityKind, JournalEntryId};
use crate::{contexts::classification::public::CategoryId, shared_kernel::CurrencyCode};
use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::Decimal;
use serde::Serialize;
#[derive(Clone, Debug)]
pub enum AnalyticsCategories {
    All,
    Assigned(Vec<CategoryId>),
    Uncategorized,
}
#[derive(Clone, Debug)]
pub struct AnalyticsFilter {
    pub currency: CurrencyCode,
    pub categories: AnalyticsCategories,
}
#[derive(Clone, Debug)]
pub struct AnalyticsInterval {
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct AnalyticsTotals {
    #[serde(with = "rust_decimal::serde::str")]
    pub income: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub expenses: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub net: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub purchases: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub expense_credits: Decimal,
    pub income_count: i64,
    pub expense_count: i64,
    pub transaction_count: i64,
}
impl AnalyticsTotals {
    pub fn add(&mut self, other: &Self) {
        self.income += other.income;
        self.expenses += other.expenses;
        self.net += other.net;
        self.purchases += other.purchases;
        self.expense_credits += other.expense_credits;
        self.income_count += other.income_count;
        self.expense_count += other.expense_count;
        self.transaction_count += other.transaction_count;
    }
}
#[derive(Clone, Debug)]
pub struct AnalyticsFact {
    pub interval: usize,
    pub category_id: Option<CategoryId>,
    pub totals: AnalyticsTotals,
    /// Only expense-bearing journals, including the income side of mixed journals.
    pub expense_totals: AnalyticsTotals,
}
#[derive(Clone, Debug, Serialize)]
pub struct AnalyticsBucket {
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
    pub label_date: NaiveDate,
    pub partial: bool,
    pub totals: AnalyticsTotals,
}
#[derive(Clone, Debug)]
pub struct AnalyticsCalendarRequest {
    pub range: AnalyticsInterval,
    pub comparison: Option<AnalyticsInterval>,
    pub timezone: String,
    pub trend_months: u32,
}
#[derive(Clone, Debug)]
pub struct AnalyticsCalendar {
    pub granularity: String,
    pub series: Vec<AnalyticsBucket>,
    pub trend: Vec<AnalyticsBucket>,
}
#[derive(Clone, Debug)]
pub struct AnalyticsTransactionsQuery {
    pub filter: AnalyticsFilter,
    pub range: AnalyticsInterval,
    pub kind: ActivityKind,
    pub after: Option<ActivityCursor>,
    pub limit: u32,
}
#[derive(Clone, Debug, Serialize)]
pub struct AnalyticsTransaction {
    pub journal_entry_id: JournalEntryId,
    pub ledger_sequence: i64,
    pub occurred_at: DateTime<Utc>,
    pub description: String,
    pub category_id: Option<CategoryId>,
    #[serde(with = "rust_decimal::serde::str")]
    pub income: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub expenses: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub net: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub contribution: Decimal,
}
#[derive(Clone, Debug, Serialize)]
pub struct AnalyticsSummary {
    pub transaction_count: i64,
    #[serde(with = "rust_decimal::serde::str")]
    pub contribution: Decimal,
}
#[derive(Clone, Debug, Serialize)]
pub struct AnalyticsPage {
    #[serde(skip)]
    pub assigned_category_ids: Vec<CategoryId>,
    pub summary: AnalyticsSummary,
    pub items: Vec<AnalyticsTransaction>,
    pub next_cursor: Option<ActivityCursor>,
}
