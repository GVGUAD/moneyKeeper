use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::Decimal;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Granularity {
    Day,
    Month,
    Year,
}

impl Granularity {
    pub fn as_sql(&self) -> &'static str {
        match self {
            Granularity::Day => "day",
            Granularity::Month => "month",
            Granularity::Year => "year",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "day" => Some(Self::Day),
            "month" => Some(Self::Month),
            "year" => Some(Self::Year),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct MissingRate {
    pub date: NaiveDate,
    pub currency: String,
}

#[derive(Debug, Clone)]
pub struct StatsRange {
    pub user_id: Uuid,
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
    pub base_currency: String,
}

#[derive(Debug, Clone)]
pub struct DashboardStats {
    pub net_worth: Decimal,
    pub month_income: Decimal,
    pub month_expense: Decimal,
    pub top_categories: Vec<CategoryBreakdownItem>,
    pub missing_rates: Vec<MissingRate>,
}

#[derive(Debug, Clone)]
pub struct BalanceHistoryPoint {
    pub period_start: DateTime<Utc>,
    pub balance: Decimal,
}

#[derive(Debug, Clone)]
pub struct CashflowPoint {
    pub period_start: DateTime<Utc>,
    pub income: Decimal,
    pub expense: Decimal,
}

#[derive(Debug, Clone)]
pub struct CategoryBreakdownItem {
    pub category_id: Option<Uuid>,
    pub name: String,
    pub total: Decimal,
}

#[derive(Debug, Clone)]
pub struct TickerHolding {
    pub ticker: String,
    pub holdings: Decimal,
    pub cost_basis: Decimal,
    pub realized_pnl: Decimal,
    pub staking_received: Decimal,
}

#[derive(Debug, Clone)]
pub struct TickerTradeLeg {
    pub ticker: String,
    pub kind: String,
    pub quantity: Decimal,
    pub amount_in_base: Decimal,
    pub transacted_at: DateTime<Utc>,
}

#[async_trait::async_trait]
pub trait StatsRepository: Send + Sync {
    async fn dashboard(
        &self,
        user_id: Uuid,
        base_currency: &str,
        top_n: i64,
    ) -> anyhow::Result<DashboardStats>;

    async fn balance_history(
        &self,
        range: &StatsRange,
        granularity: Granularity,
    ) -> anyhow::Result<(Vec<BalanceHistoryPoint>, Vec<MissingRate>)>;

    async fn cashflow(
        &self,
        range: &StatsRange,
        granularity: Granularity,
    ) -> anyhow::Result<(Vec<CashflowPoint>, Vec<MissingRate>)>;

    async fn categories(
        &self,
        range: &StatsRange,
        kind: &str,
    ) -> anyhow::Result<(Vec<CategoryBreakdownItem>, Vec<MissingRate>)>;

    async fn investment_trades(
        &self,
        user_id: Uuid,
        base_currency: &str,
    ) -> anyhow::Result<(Vec<TickerTradeLeg>, Vec<MissingRate>)>;
}
