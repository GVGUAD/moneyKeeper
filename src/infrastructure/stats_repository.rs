use async_trait::async_trait;
use chrono::{Datelike, TimeZone, Utc};
use rust_decimal::Decimal;
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::domain::stats::{
    BalanceHistoryPoint, CashflowPoint, CategoryBreakdownItem, DashboardStats, Granularity,
    MissingRate, StatsRange, StatsRepository, TickerTradeLeg,
};

pub struct PgStatsRepository {
    pool: PgPool,
}

impl PgStatsRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

// Transfer convention: only the source-side `transactions` row is stored
// (the destination is never inserted by current handlers/sync code).
// We sign it as -amount, matching the existing `signed_delta` behavior
// used by TransactionService for account balance maintenance.

// Helper: a SQL fragment expressing per-row conversion to base currency.
// `amount_col` is the column/expression to convert, `currency_col` is the row
// currency column, `base_param` is the SQL placeholder for the base currency
// string, `to_uah_col` is the precomputed from_currency→UAH rate (nullable),
// and `base_to_uah_col` is the precomputed base→UAH rate at the same date (nullable).
fn conversion_case(
    amount_col: &str,
    currency_col: &str,
    base_param: &str,
    to_uah_col: &str,
    base_to_uah_col: &str,
) -> String {
    format!(
        "CASE
            WHEN {currency_col} = {base_param} THEN {amount_col}
            WHEN {currency_col} = 'UAH' AND {base_param} = 'UAH' THEN {amount_col}
            WHEN {currency_col} = 'UAH' AND {base_to_uah_col} IS NOT NULL
                THEN {amount_col} / {base_to_uah_col}
            WHEN {base_param} = 'UAH' AND {to_uah_col} IS NOT NULL
                THEN {amount_col} * {to_uah_col}
            WHEN {to_uah_col} IS NOT NULL AND {base_to_uah_col} IS NOT NULL
                THEN {amount_col} * {to_uah_col} / {base_to_uah_col}
            ELSE NULL
        END"
    )
}

#[async_trait]
impl StatsRepository for PgStatsRepository {
    // ─── categories ──────────────────────────────────────────────────────────

    async fn categories(
        &self,
        range: &StatsRange,
        kind: &str,
    ) -> anyhow::Result<(Vec<CategoryBreakdownItem>, Vec<MissingRate>)> {
        let conv = conversion_case("t.amount", "t.currency", "$4", "r.to_uah", "r.base_to_uah");

        let sql = format!(
            r#"
            WITH rates AS (
                SELECT
                    s.tx_date,
                    s.currency,
                    (SELECT rate FROM fx_rates
                     WHERE from_currency = s.currency AND to_currency = 'UAH'
                       AND rate_date <= s.tx_date
                     ORDER BY rate_date DESC LIMIT 1) AS to_uah,
                    (SELECT rate FROM fx_rates
                     WHERE from_currency = $4 AND to_currency = 'UAH'
                       AND rate_date <= s.tx_date
                     ORDER BY rate_date DESC LIMIT 1) AS base_to_uah
                FROM (
                    SELECT DISTINCT transacted_at::date AS tx_date, currency
                    FROM transactions
                    WHERE user_id = $1 AND kind = $5
                      AND transacted_at >= $2 AND transacted_at < $3
                ) s
            )
            SELECT
                t.category_id,
                COALESCE(c.name, 'Uncategorized') AS name,
                SUM({conv}) AS total
            FROM transactions t
            LEFT JOIN categories c ON c.id = t.category_id
            LEFT JOIN rates r ON r.tx_date = t.transacted_at::date AND r.currency = t.currency
            WHERE t.user_id = $1 AND t.kind = $5
              AND t.transacted_at >= $2 AND t.transacted_at < $3
            GROUP BY t.category_id, c.name
            HAVING SUM({conv}) IS NOT NULL
            ORDER BY total DESC
            "#,
            conv = conv,
        );

        let rows = sqlx::query(&sql)
            .bind(range.user_id)
            .bind(range.from)
            .bind(range.to)
            .bind(&range.base_currency)
            .bind(kind)
            .fetch_all(&self.pool)
            .await?;

        let items = rows
            .iter()
            .map(|r| {
                Ok(CategoryBreakdownItem {
                    category_id: r.try_get("category_id")?,
                    name: r.try_get("name")?,
                    total: r.try_get("total")?,
                })
            })
            .collect::<anyhow::Result<Vec<_>>>()?;

        // Missing rates: (date, currency) pairs where conversion would be NULL
        let missing_sql = r#"
            SELECT DISTINCT t.transacted_at::date AS rate_date, t.currency
            FROM transactions t
            WHERE t.user_id = $1 AND t.kind = $5
              AND t.transacted_at >= $2 AND t.transacted_at < $3
              AND t.currency != $4
              AND NOT EXISTS (
                  SELECT 1 FROM fx_rates
                  WHERE from_currency = t.currency AND to_currency = 'UAH'
                    AND rate_date <= t.transacted_at::date
              )
            ORDER BY rate_date, currency
            LIMIT 10
        "#;

        let missing_rows = sqlx::query(missing_sql)
            .bind(range.user_id)
            .bind(range.from)
            .bind(range.to)
            .bind(&range.base_currency)
            .bind(kind)
            .fetch_all(&self.pool)
            .await?;

        let missing = missing_rows
            .iter()
            .map(|r| {
                Ok(MissingRate {
                    date: r.try_get("rate_date")?,
                    currency: r.try_get("currency")?,
                })
            })
            .collect::<anyhow::Result<Vec<_>>>()?;

        Ok((items, missing))
    }

    // ─── cashflow ─────────────────────────────────────────────────────────────

    async fn cashflow(
        &self,
        range: &StatsRange,
        granularity: Granularity,
    ) -> anyhow::Result<(Vec<CashflowPoint>, Vec<MissingRate>)> {
        let gran = granularity.as_sql();
        let conv = conversion_case("t.amount", "t.currency", "$4", "r.to_uah", "r.base_to_uah");

        let sql = format!(
            r#"
            WITH rates AS (
                SELECT
                    s.tx_date,
                    s.currency,
                    (SELECT rate FROM fx_rates
                     WHERE from_currency = s.currency AND to_currency = 'UAH'
                       AND rate_date <= s.tx_date
                     ORDER BY rate_date DESC LIMIT 1) AS to_uah,
                    (SELECT rate FROM fx_rates
                     WHERE from_currency = $4 AND to_currency = 'UAH'
                       AND rate_date <= s.tx_date
                     ORDER BY rate_date DESC LIMIT 1) AS base_to_uah
                FROM (
                    SELECT DISTINCT transacted_at::date AS tx_date, currency
                    FROM transactions
                    WHERE user_id = $1 AND kind IN ('Income', 'Expense')
                      AND transacted_at >= $2 AND transacted_at < $3
                ) s
            )
            SELECT
                date_trunc('{gran}', t.transacted_at) AS period_start,
                COALESCE(SUM(CASE WHEN t.kind = 'Income' THEN {conv} ELSE 0 END), 0) AS income,
                COALESCE(SUM(CASE WHEN t.kind = 'Expense' THEN {conv} ELSE 0 END), 0) AS expense
            FROM transactions t
            LEFT JOIN rates r ON r.tx_date = t.transacted_at::date AND r.currency = t.currency
            WHERE t.user_id = $1 AND t.kind IN ('Income', 'Expense')
              AND t.transacted_at >= $2 AND t.transacted_at < $3
            GROUP BY period_start
            ORDER BY period_start
            "#,
            gran = gran,
            conv = conv,
        );

        let rows = sqlx::query(&sql)
            .bind(range.user_id)
            .bind(range.from)
            .bind(range.to)
            .bind(&range.base_currency)
            .fetch_all(&self.pool)
            .await?;

        let points = rows
            .iter()
            .map(|r| {
                Ok(CashflowPoint {
                    period_start: r.try_get("period_start")?,
                    income: r.try_get("income")?,
                    expense: r.try_get("expense")?,
                })
            })
            .collect::<anyhow::Result<Vec<_>>>()?;

        // Missing rates
        let missing_sql = r#"
            SELECT DISTINCT t.transacted_at::date AS rate_date, t.currency
            FROM transactions t
            WHERE t.user_id = $1 AND t.kind IN ('Income', 'Expense')
              AND t.transacted_at >= $2 AND t.transacted_at < $3
              AND t.currency != $4
              AND NOT EXISTS (
                  SELECT 1 FROM fx_rates
                  WHERE from_currency = t.currency AND to_currency = 'UAH'
                    AND rate_date <= t.transacted_at::date
              )
            ORDER BY rate_date, currency
            LIMIT 10
        "#;

        let missing_rows = sqlx::query(missing_sql)
            .bind(range.user_id)
            .bind(range.from)
            .bind(range.to)
            .bind(&range.base_currency)
            .fetch_all(&self.pool)
            .await?;

        let missing = missing_rows
            .iter()
            .map(|r| {
                Ok(MissingRate {
                    date: r.try_get("rate_date")?,
                    currency: r.try_get("currency")?,
                })
            })
            .collect::<anyhow::Result<Vec<_>>>()?;

        Ok((points, missing))
    }

    // ─── balance_history ─────────────────────────────────────────────────────

    // Implementation note: seed = 0.
    // `accounts.balance` reflects the *current* balance after all transactions.
    // We can't recover the true opening balance without a separate column,
    // so we use the running sum of signed deltas. The current `accounts.balance`
    // will equal this sum if all transactions are recorded.
    async fn balance_history(
        &self,
        range: &StatsRange,
        granularity: Granularity,
    ) -> anyhow::Result<(Vec<BalanceHistoryPoint>, Vec<MissingRate>)> {
        let gran = granularity.as_sql();

        let signed_amount = "CASE
            WHEN t.kind IN ('Income','Sell','StakingReward') THEN t.amount
            WHEN t.kind IN ('Expense','Buy','Transfer')      THEN -t.amount
            ELSE 0
        END";
        let conv = conversion_case(
            signed_amount,
            "t.currency",
            "$2",
            "r.to_uah",
            "r.base_to_uah",
        );

        // Query 1: pre_total — sum of all signed deltas before range.from
        let pre_sql = format!(
            r#"
            WITH rates AS (
                SELECT
                    s.tx_date,
                    s.currency,
                    (SELECT rate FROM fx_rates
                     WHERE from_currency = s.currency AND to_currency = 'UAH'
                       AND rate_date <= s.tx_date
                     ORDER BY rate_date DESC LIMIT 1) AS to_uah,
                    (SELECT rate FROM fx_rates
                     WHERE from_currency = $2 AND to_currency = 'UAH'
                       AND rate_date <= s.tx_date
                     ORDER BY rate_date DESC LIMIT 1) AS base_to_uah
                FROM (
                    SELECT DISTINCT transacted_at::date AS tx_date, currency
                    FROM transactions
                    WHERE user_id = $1 AND transacted_at < $3
                ) s
            )
            SELECT COALESCE(SUM({conv}), 0) AS pre_total
            FROM transactions t
            LEFT JOIN rates r ON r.tx_date = t.transacted_at::date AND r.currency = t.currency
            WHERE t.user_id = $1 AND t.transacted_at < $3
            "#,
            conv = conv,
        );

        let pre_total: Decimal = sqlx::query_scalar(&pre_sql)
            .bind(range.user_id)
            .bind(&range.base_currency)
            .bind(range.from)
            .fetch_one(&self.pool)
            .await?;

        // Query 2: bucketed deltas within range
        let bucket_sql = format!(
            r#"
            WITH rates AS (
                SELECT
                    s.tx_date,
                    s.currency,
                    (SELECT rate FROM fx_rates
                     WHERE from_currency = s.currency AND to_currency = 'UAH'
                       AND rate_date <= s.tx_date
                     ORDER BY rate_date DESC LIMIT 1) AS to_uah,
                    (SELECT rate FROM fx_rates
                     WHERE from_currency = $2 AND to_currency = 'UAH'
                       AND rate_date <= s.tx_date
                     ORDER BY rate_date DESC LIMIT 1) AS base_to_uah
                FROM (
                    SELECT DISTINCT transacted_at::date AS tx_date, currency
                    FROM transactions
                    WHERE user_id = $1 AND transacted_at >= $3 AND transacted_at < $4
                ) s
            )
            SELECT
                date_trunc('{gran}', t.transacted_at) AS bucket,
                COALESCE(SUM({conv}), 0) AS delta
            FROM transactions t
            LEFT JOIN rates r ON r.tx_date = t.transacted_at::date AND r.currency = t.currency
            WHERE t.user_id = $1 AND t.transacted_at >= $3 AND t.transacted_at < $4
            GROUP BY bucket
            ORDER BY bucket
            "#,
            gran = gran,
            conv = conv,
        );

        let rows = sqlx::query(&bucket_sql)
            .bind(range.user_id)
            .bind(&range.base_currency)
            .bind(range.from)
            .bind(range.to)
            .fetch_all(&self.pool)
            .await?;

        // Compute running total in Rust, starting from pre_total
        let mut running = pre_total;
        let mut points = Vec::with_capacity(rows.len());
        for row in &rows {
            let bucket: chrono::DateTime<Utc> = row.try_get("bucket")?;
            let delta: Decimal = row.try_get("delta")?;
            running += delta;
            points.push(BalanceHistoryPoint {
                period_start: bucket,
                balance: running,
            });
        }

        // Missing rates — cover entire user history up to range.to (both pre and in-range)
        let missing_sql = r#"
            SELECT DISTINCT t.transacted_at::date AS rate_date, t.currency
            FROM transactions t
            WHERE t.user_id = $1 AND t.transacted_at < $3
              AND t.currency != $2
              AND NOT EXISTS (
                  SELECT 1 FROM fx_rates
                  WHERE from_currency = t.currency AND to_currency = 'UAH'
                    AND rate_date <= t.transacted_at::date
              )
            ORDER BY rate_date, currency
            LIMIT 10
        "#;

        let missing_rows = sqlx::query(missing_sql)
            .bind(range.user_id)
            .bind(&range.base_currency)
            .bind(range.to)
            .fetch_all(&self.pool)
            .await?;

        let missing = missing_rows
            .iter()
            .map(|r| {
                Ok(MissingRate {
                    date: r.try_get("rate_date")?,
                    currency: r.try_get("currency")?,
                })
            })
            .collect::<anyhow::Result<Vec<_>>>()?;

        Ok((points, missing))
    }

    // ─── investment_trades ───────────────────────────────────────────────────

    async fn investment_trades(
        &self,
        user_id: Uuid,
        base_currency: &str,
    ) -> anyhow::Result<(Vec<TickerTradeLeg>, Vec<MissingRate>)> {
        let conv = conversion_case("t.amount", "t.currency", "$2", "r.to_uah", "r.base_to_uah");

        let sql = format!(
            r#"
            WITH rates AS (
                SELECT
                    s.tx_date,
                    s.currency,
                    (SELECT rate FROM fx_rates
                     WHERE from_currency = s.currency AND to_currency = 'UAH'
                       AND rate_date <= s.tx_date
                     ORDER BY rate_date DESC LIMIT 1) AS to_uah,
                    (SELECT rate FROM fx_rates
                     WHERE from_currency = $2 AND to_currency = 'UAH'
                       AND rate_date <= s.tx_date
                     ORDER BY rate_date DESC LIMIT 1) AS base_to_uah
                FROM (
                    SELECT DISTINCT transacted_at::date AS tx_date, currency
                    FROM transactions
                    WHERE user_id = $1 AND kind IN ('Buy', 'Sell', 'StakingReward')
                ) s
            )
            SELECT
                td.ticker,
                t.kind,
                td.quantity,
                {conv} AS amount_in_base,
                t.transacted_at
            FROM transactions t
            JOIN trade_details td ON td.transaction_id = t.id
            LEFT JOIN rates r ON r.tx_date = t.transacted_at::date AND r.currency = t.currency
            WHERE t.user_id = $1 AND t.kind IN ('Buy', 'Sell', 'StakingReward')
            ORDER BY td.ticker, t.transacted_at
            "#,
            conv = conv,
        );

        let rows = sqlx::query(&sql)
            .bind(user_id)
            .bind(base_currency)
            .fetch_all(&self.pool)
            .await?;

        let legs = rows
            .iter()
            .map(|r| {
                Ok(TickerTradeLeg {
                    ticker: r.try_get("ticker")?,
                    kind: r.try_get("kind")?,
                    quantity: r.try_get("quantity")?,
                    amount_in_base: r
                        .try_get::<Option<Decimal>, _>("amount_in_base")?
                        .unwrap_or(Decimal::ZERO),
                    transacted_at: r.try_get("transacted_at")?,
                })
            })
            .collect::<anyhow::Result<Vec<_>>>()?;

        // Missing rates
        let missing_sql = r#"
            SELECT DISTINCT t.transacted_at::date AS rate_date, t.currency
            FROM transactions t
            WHERE t.user_id = $1 AND t.kind IN ('Buy', 'Sell', 'StakingReward')
              AND t.currency != $2
              AND NOT EXISTS (
                  SELECT 1 FROM fx_rates
                  WHERE from_currency = t.currency AND to_currency = 'UAH'
                    AND rate_date <= t.transacted_at::date
              )
            ORDER BY rate_date, currency
            LIMIT 10
        "#;

        let missing_rows = sqlx::query(missing_sql)
            .bind(user_id)
            .bind(base_currency)
            .fetch_all(&self.pool)
            .await?;

        let missing = missing_rows
            .iter()
            .map(|r| {
                Ok(MissingRate {
                    date: r.try_get("rate_date")?,
                    currency: r.try_get("currency")?,
                })
            })
            .collect::<anyhow::Result<Vec<_>>>()?;

        Ok((legs, missing))
    }

    // ─── dashboard ───────────────────────────────────────────────────────────

    async fn dashboard(
        &self,
        user_id: Uuid,
        base_currency: &str,
        top_n: i64,
    ) -> anyhow::Result<DashboardStats> {
        use chrono::DateTime;

        let now = Utc::now();
        let month_start = Utc
            .with_ymd_and_hms(now.year(), now.month(), 1, 0, 0, 0)
            .unwrap();
        let month_end = if now.month() == 12 {
            Utc.with_ymd_and_hms(now.year() + 1, 1, 1, 0, 0, 0).unwrap()
        } else {
            Utc.with_ymd_and_hms(now.year(), now.month() + 1, 1, 0, 0, 0)
                .unwrap()
        };

        // Net worth: running sum of all signed deltas from the beginning of time
        let epoch: DateTime<Utc> = DateTime::from_timestamp(0, 0).unwrap();
        let net_worth_range = StatsRange {
            user_id,
            from: epoch,
            to: now,
            base_currency: base_currency.to_string(),
        };
        let (history, mut missing_rates) = self
            .balance_history(&net_worth_range, Granularity::Year)
            .await?;
        let net_worth = history.last().map(|p| p.balance).unwrap_or(Decimal::ZERO);

        // Month cashflow
        let month_range = StatsRange {
            user_id,
            from: month_start,
            to: month_end,
            base_currency: base_currency.to_string(),
        };
        let (cashflow_pts, mut cf_missing) =
            self.cashflow(&month_range, Granularity::Month).await?;
        missing_rates.append(&mut cf_missing);

        let month_income: Decimal = cashflow_pts.iter().map(|p| p.income).sum();
        let month_expense: Decimal = cashflow_pts.iter().map(|p| p.expense).sum();

        // Top categories (Expense, this month)
        let (mut cat_items, mut cat_missing) = self.categories(&month_range, "Expense").await?;
        missing_rates.append(&mut cat_missing);

        cat_items.truncate(top_n as usize);

        // Dedup and truncate missing rates
        missing_rates.sort();
        missing_rates.dedup();
        missing_rates.truncate(10);

        Ok(DashboardStats {
            net_worth,
            month_income,
            month_expense,
            top_categories: cat_items,
            missing_rates,
        })
    }
}
