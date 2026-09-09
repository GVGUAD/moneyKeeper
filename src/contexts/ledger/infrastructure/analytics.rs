//! Live analytics use surviving ordinary journals, with one fact per journal/currency.
use crate::contexts::classification::public::CategoryId;
use crate::contexts::ledger::public::*;
use crate::shared_kernel::UserId;
use chrono::{DateTime, Utc};
use sqlx::{PgPool, Postgres, QueryBuilder, Row};
use uuid::Uuid;

// Both reads bind the exact union of intervals before touching postings. Reversal
// lookup deliberately has no occurrence bound: corrections restate old periods.
fn facts(
    user: UserId,
    filter: &AnalyticsFilter,
    intervals: &[AnalyticsInterval],
) -> QueryBuilder<'static, Postgres> {
    let mut q = QueryBuilder::new("WITH ranges AS (SELECT * FROM unnest(");
    q.push_bind(intervals.iter().map(|r| r.from).collect::<Vec<_>>())
        .push("::timestamptz[], ")
        .push_bind(intervals.iter().map(|r| r.to).collect::<Vec<_>>())
        .push(
            r#"::timestamptz[]) WITH ORDINALITY AS r(start_at,end_at,ordinal)),
        effective AS (
            SELECT j.id, j.ledger_sequence, j.occurred_at,
                   COALESCE(a.description,j.description) AS description,
                   a.category_id, j.user_id
            FROM ledger.journal_entries j
            LEFT JOIN ledger.transaction_annotations a
              ON a.user_id=j.user_id AND a.journal_entry_id=j.id
            WHERE j.user_id="#,
        )
        .push_bind(user.into_uuid())
        .push(
            r#" AND j.purpose='ordinary' AND j.reverses_transaction_id IS NULL
            AND NOT EXISTS (
                SELECT 1 FROM ledger.journal_entries later
                WHERE later.user_id=j.user_id
                  AND later.reverses_transaction_id=j.id
            )
            AND NOT EXISTS (
                SELECT 1 FROM ledger.journal_entries replacement
                WHERE replacement.user_id=j.user_id AND replacement.replaces_transaction_id=j.id
            )"#,
        );
    // Merge only intersecting/adjacent ranges, preserving gaps between a distant
    // comparison period and the trend. Explicit bounds permit the time index.
    let mut ordered = intervals.to_vec();
    ordered.sort_by_key(|r| r.from);
    let mut union: Vec<AnalyticsInterval> = Vec::new();
    for range in ordered {
        if let Some(last) = union.last_mut().filter(|last| range.from <= last.to) {
            last.to = last.to.max(range.to);
        } else {
            union.push(range);
        }
    }
    q.push(" AND (");
    for (index, range) in union.iter().enumerate() {
        if index > 0 {
            q.push(" OR ");
        }
        q.push("(j.occurred_at>=")
            .push_bind(range.from)
            .push(" AND j.occurred_at<")
            .push_bind(range.to)
            .push(")");
    }
    q.push(")");
    match &filter.categories {
        AnalyticsCategories::All => {}
        AnalyticsCategories::Uncategorized => {
            q.push(" AND a.category_id IS NULL");
        }
        AnalyticsCategories::Assigned(ids) => {
            q.push(" AND a.category_id=ANY(")
                .push_bind(ids.iter().map(|id| id.into_uuid()).collect::<Vec<_>>())
                .push("::uuid[])");
        }
    }
    q.push(
        r#"), components AS (
        SELECT e.id,e.ledger_sequence,e.occurred_at,e.description,e.category_id,
               -COALESCE(SUM(p.signed_amount) FILTER(WHERE p.account_nature='income'),0) AS income,
               COALESCE(SUM(p.signed_amount) FILTER(WHERE p.account_nature='expense'),0) AS expenses
        FROM effective e JOIN ledger.postings p
          ON p.user_id=e.user_id AND p.journal_entry_id=e.id
        WHERE p.currency="#,
    )
    .push_bind(filter.currency.as_str().to_owned())
    .push(
        r#" AND p.account_nature IN ('income','expense')
        GROUP BY e.id,e.ledger_sequence,e.occurred_at,e.description,e.category_id
    ), facts AS (
        SELECT *,income-expenses AS net FROM components WHERE income<>0 OR expenses<>0
    ) "#,
    );
    q
}
const TOTALS: &str = r#"
    COALESCE(SUM(income),0) AS income,
    COALESCE(SUM(expenses),0) AS expenses,
    COALESCE(SUM(net),0) AS net,
    COALESCE(SUM(GREATEST(expenses,0)),0) AS purchases,
    COALESCE(SUM(GREATEST(-expenses,0)),0) AS expense_credits,
    COUNT(*) FILTER(WHERE income<>0) AS income_count,
    COUNT(*) FILTER(WHERE expenses<>0) AS expense_count,
    COUNT(*) AS transaction_count"#;
fn totals(row: &sqlx::postgres::PgRow) -> Result<AnalyticsTotals, sqlx::Error> {
    Ok(AnalyticsTotals {
        income: row.try_get("income")?,
        expenses: row.try_get("expenses")?,
        net: row.try_get("net")?,
        purchases: row.try_get("purchases")?,
        expense_credits: row.try_get("expense_credits")?,
        income_count: row.try_get("income_count")?,
        expense_count: row.try_get("expense_count")?,
        transaction_count: row.try_get("transaction_count")?,
    })
}
async fn snapshot(pool: &PgPool) -> Result<sqlx::Transaction<'_, Postgres>, LedgerError> {
    let mut tx = pool.begin().await.map_err(LedgerError::storage)?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
        .execute(&mut *tx)
        .await
        .map_err(LedgerError::storage)?;
    Ok(tx)
}
fn validate_intervals(intervals: &[AnalyticsInterval]) -> Result<(), LedgerError> {
    if intervals.is_empty() || intervals.len() > 800 || intervals.iter().any(|r| r.from >= r.to) {
        return Err(LedgerError::invalid_state("invalid analytics intervals"));
    }
    Ok(())
}
pub(super) async fn analytics_aggregate(
    pool: &PgPool,
    user: UserId,
    filter: AnalyticsFilter,
    intervals: Vec<AnalyticsInterval>,
) -> Result<Vec<AnalyticsFact>, LedgerError> {
    validate_intervals(&intervals)?;
    let mut tx = snapshot(pool).await?;
    let mut q = facts(user, &filter, &intervals);
    q.push("SELECT r.ordinal,f.category_id,").push(TOTALS)
        .push(",COALESCE(SUM(income) FILTER(WHERE expenses<>0),0) AS spending_income, COUNT(*) FILTER(WHERE expenses<>0 AND income<>0) AS spending_income_count")
        .push(" FROM facts f JOIN ranges r ON f.occurred_at>=r.start_at AND f.occurred_at<r.end_at GROUP BY r.ordinal,f.category_id ORDER BY r.ordinal");
    let rows = q
        .build()
        .fetch_all(&mut *tx)
        .await
        .map_err(LedgerError::storage)?;
    let result = rows
        .iter()
        .map(|row| {
            let totals = totals(row)?;
            let mut expense_totals = totals.clone();
            expense_totals.income = row.try_get("spending_income")?;
            expense_totals.net = expense_totals.income - expense_totals.expenses;
            expense_totals.income_count = row.try_get("spending_income_count")?;
            expense_totals.transaction_count = expense_totals.expense_count;
            Ok(AnalyticsFact {
                interval: (row.try_get::<i64, _>("ordinal")? - 1) as usize,
                category_id: row
                    .try_get::<Option<Uuid>, _>("category_id")?
                    .map(CategoryId::from_uuid),
                totals,
                expense_totals,
            })
        })
        .collect::<Result<Vec<_>, sqlx::Error>>()
        .map_err(LedgerError::storage)?;
    tx.commit().await.map_err(LedgerError::storage)?;
    Ok(result)
}
pub(super) async fn analytics_transactions(
    pool: &PgPool,
    user: UserId,
    query: AnalyticsTransactionsQuery,
) -> Result<AnalyticsPage, LedgerError> {
    validate_intervals(std::slice::from_ref(&query.range))?;
    if !(1..=200).contains(&query.limit) {
        return Err(LedgerError::invalid_state("limit must be 1 to 200"));
    }
    let mut tx = snapshot(pool).await?;
    let (predicate, contribution) = match query.kind {
        ActivityKind::All => ("TRUE", "net"),
        ActivityKind::Income => ("income<>0", "income"),
        ActivityKind::Expense => ("expenses<>0", "-expenses"),
    };
    let mut q = facts(user, &query.filter, std::slice::from_ref(&query.range));
    q.push("SELECT array_agg(DISTINCT category_id) FILTER (WHERE category_id IS NOT NULL) AS category_ids,COUNT(*) AS transaction_count,COALESCE(SUM(").push(contribution).push("),0) AS contribution FROM facts WHERE ").push(predicate);
    let row = q
        .build()
        .fetch_one(&mut *tx)
        .await
        .map_err(LedgerError::storage)?;
    let assigned_category_ids = row
        .try_get::<Option<Vec<Uuid>>, _>("category_ids")
        .map_err(LedgerError::storage)?
        .unwrap_or_default()
        .into_iter()
        .map(CategoryId::from_uuid)
        .collect();
    let summary = AnalyticsSummary {
        transaction_count: row
            .try_get("transaction_count")
            .map_err(LedgerError::storage)?,
        contribution: row.try_get("contribution").map_err(LedgerError::storage)?,
    };
    let mut q = facts(user, &query.filter, std::slice::from_ref(&query.range));
    q.push("SELECT *,")
        .push(contribution)
        .push(" AS contribution FROM facts WHERE ")
        .push(predicate);
    if let Some(after) = query.after {
        q.push(" AND (occurred_at,ledger_sequence)<(")
            .push_bind(after.occurred_at)
            .push(",")
            .push_bind(after.ledger_sequence)
            .push(")");
    }
    q.push(" ORDER BY occurred_at DESC,ledger_sequence DESC LIMIT ")
        .push_bind(i64::from(query.limit) + 1);
    let rows = q
        .build()
        .fetch_all(&mut *tx)
        .await
        .map_err(LedgerError::storage)?;
    let more = rows.len() > query.limit as usize;
    let items = rows
        .iter()
        .take(query.limit as usize)
        .map(|r| {
            Ok(AnalyticsTransaction {
                journal_entry_id: JournalEntryId::from_uuid(r.try_get("id")?),
                ledger_sequence: r.try_get("ledger_sequence")?,
                occurred_at: r.try_get("occurred_at")?,
                description: r.try_get("description")?,
                category_id: r
                    .try_get::<Option<Uuid>, _>("category_id")?
                    .map(CategoryId::from_uuid),
                income: r.try_get("income")?,
                expenses: r.try_get("expenses")?,
                net: r.try_get("net")?,
                contribution: r.try_get("contribution")?,
            })
        })
        .collect::<Result<Vec<_>, sqlx::Error>>()
        .map_err(LedgerError::storage)?;
    let next_cursor = if more {
        items.last().map(|r| ActivityCursor {
            occurred_at: r.occurred_at,
            ledger_sequence: r.ledger_sequence,
        })
    } else {
        None
    };
    tx.commit().await.map_err(LedgerError::storage)?;
    Ok(AnalyticsPage {
        assigned_category_ids,
        summary,
        items,
        next_cursor,
    })
}
pub(super) async fn analytics_calendar(
    pool: &PgPool,
    request: AnalyticsCalendarRequest,
) -> Result<AnalyticsCalendar, LedgerError> {
    let valid: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_timezone_names WHERE name=$1)")
            .bind(&request.timezone)
            .fetch_one(pool)
            .await
            .map_err(LedgerError::storage)?;
    if !valid {
        return Err(LedgerError::invalid_state("invalid timezone"));
    }
    if ![6, 12].contains(&request.trend_months) {
        return Err(LedgerError::invalid_state("trend_months must be 6 or 12"));
    }
    let mut days = 0;
    for range in std::iter::once(&request.range).chain(request.comparison.iter()) {
        validate_intervals(std::slice::from_ref(range))?;
        // Subtract one microsecond from the exclusive instant BEFORE conversion,
        // including during a repeated local hour at the autumn DST transition.
        let count:i32=sqlx::query_scalar("SELECT (($2::timestamptz-interval '1 microsecond') AT TIME ZONE $3)::date-($1::timestamptz AT TIME ZONE $3)::date+1").bind(range.from).bind(range.to).bind(&request.timezone).fetch_one(pool).await.map_err(LedgerError::storage)?;
        if !(1..=366).contains(&count) {
            return Err(LedgerError::invalid_state(
                "range may touch at most 366 local dates",
            ));
        }
        if days == 0 {
            days = count;
        }
    }
    let granularity = if days <= 31 {
        "day"
    } else if days <= 93 {
        "week"
    } else {
        "month"
    };
    let series = buckets(
        pool,
        request.range.from,
        request.range.to,
        &request.timezone,
        granularity,
    )
    .await?;
    let trend_from:DateTime<Utc>=sqlx::query_scalar("SELECT (date_trunc('month',($1::timestamptz-interval '1 microsecond') AT TIME ZONE $2)-make_interval(months => $3)) AT TIME ZONE $2").bind(request.range.to).bind(&request.timezone).bind(request.trend_months as i32-1).fetch_one(pool).await.map_err(LedgerError::storage)?;
    let trend = buckets(
        pool,
        trend_from,
        request.range.to,
        &request.timezone,
        "month",
    )
    .await?;
    Ok(AnalyticsCalendar {
        granularity: granularity.into(),
        series,
        trend,
    })
}
async fn buckets(
    pool: &PgPool,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    tz: &str,
    unit: &str,
) -> Result<Vec<AnalyticsBucket>, LedgerError> {
    let rows=sqlx::query("WITH local AS (SELECT date_trunc($4,$1::timestamptz AT TIME ZONE $3) AS start_at, ($2::timestamptz-interval '1 microsecond') AT TIME ZONE $3 AS end_at), calendar_intervals AS (SELECT d AT TIME ZONE $3 AS start_at,(d+('1 '||$4)::interval) AT TIME ZONE $3 AS end_at,d::date AS label_date FROM local CROSS JOIN LATERAL generate_series(start_at,end_at,('1 '||$4)::interval) d) SELECT GREATEST(start_at,$1) AS from,LEAST(end_at,$2) AS to,label_date,(start_at<$1 OR end_at>$2) AS partial FROM calendar_intervals WHERE start_at<$2 AND end_at>$1 AND end_at>start_at ORDER BY start_at")
    .bind(from).bind(to).bind(tz).bind(unit).fetch_all(pool).await.map_err(LedgerError::storage)?;
    rows.iter()
        .map(|r| {
            Ok(AnalyticsBucket {
                from: r.try_get("from")?,
                to: r.try_get("to")?,
                label_date: r.try_get("label_date")?,
                partial: r.try_get("partial")?,
                totals: AnalyticsTotals::default(),
            })
        })
        .collect::<Result<_, sqlx::Error>>()
        .map_err(LedgerError::storage)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::{Execute, Executor};
    use testcontainers::{ImageExt, runners::AsyncRunner};
    use testcontainers_modules::postgres::Postgres as PostgresImage;

    #[tokio::test]
    async fn analytics_query_plan_and_snapshot_consistency() {
        let container = PostgresImage::default()
            .with_tag("16-alpine")
            .start()
            .await
            .unwrap();
        let port = container.get_host_port_ipv4(5432).await.unwrap();
        let database = crate::infrastructure::test_db::create_fresh_database(&format!(
            "postgres://postgres:postgres@127.0.0.1:{port}/postgres"
        ))
        .await
        .unwrap();
        let verified = database.initialize().await.unwrap();
        let pool = verified.pool();
        let user = UserId::new(Uuid::new_v4());
        let mut tx = pool.begin().await.unwrap();
        sqlx::query("INSERT INTO ledger.accounts(id,user_id,name,currency,nature,kind,authority,visibility,system_role) VALUES('00000000-0000-0000-0000-000000000001',$1,'cash','UAH','asset','system','system','hidden','fx_clearing'),('00000000-0000-0000-0000-000000000002',$1,'expenses','UAH','expense','system','system','hidden','uncategorized_expense')").bind(user.into_uuid()).execute(&mut *tx).await.unwrap();
        sqlx::query("INSERT INTO ledger.journal_entries(id,user_id,command_name,source,purpose,description,actor_kind,occurred_at,recorded_at,correlation_id,idempotency_key) SELECT gen_random_uuid(),$1,'fixture','manual','ordinary','fixture','system','2026-08-01'::timestamptz+(n%365)*interval '1 day',now(),gen_random_uuid(),n::text FROM generate_series(1,10000) n").bind(user.into_uuid()).execute(&mut *tx).await.unwrap();
        tx.execute("INSERT INTO ledger.postings(id,journal_entry_id,user_id,account_id,currency,account_nature,position,signed_amount) SELECT gen_random_uuid(),j.id,j.user_id,p.account_id,'UAH',p.nature,p.position,p.amount FROM ledger.journal_entries j CROSS JOIN (VALUES('00000000-0000-0000-0000-000000000001'::uuid,'asset',1,-1),('00000000-0000-0000-0000-000000000002'::uuid,'expense',2,1)) p(account_id,nature,position,amount)").await.unwrap();
        tx.commit().await.unwrap();
        pool.execute("ANALYZE ledger.journal_entries; ANALYZE ledger.postings")
            .await
            .unwrap();
        let filter = AnalyticsFilter {
            currency: crate::shared_kernel::CurrencyCode::new("UAH").unwrap(),
            categories: AnalyticsCategories::All,
        };
        let intervals = vec![AnalyticsInterval {
            from: "2026-08-01T00:00:00Z".parse().unwrap(),
            to: "2026-09-01T00:00:00Z".parse().unwrap(),
        }];
        let mut builder = facts(user, &filter, &intervals);
        builder.push("SELECT ").push(TOTALS).push(" FROM facts");
        let mut query = builder.build();
        let sql = format!("EXPLAIN (ANALYZE,BUFFERS,FORMAT JSON) {}", query.sql());
        let args = query.take_arguments().unwrap().unwrap();
        let plan: serde_json::Value = sqlx::query_scalar_with(&sql, args)
            .fetch_one(pool)
            .await
            .unwrap();
        println!("analytics 10,000-journal plan: {}", plan);
        // The same helper used by page/summary and aggregates must retain its
        // snapshot across a committed concurrent mutation.
        pool.execute(
            "CREATE TABLE snapshot_probe(value integer); INSERT INTO snapshot_probe VALUES(1)",
        )
        .await
        .unwrap();
        let mut read = snapshot(pool).await.unwrap();
        let before: i32 = sqlx::query_scalar("SELECT value FROM snapshot_probe")
            .fetch_one(&mut *read)
            .await
            .unwrap();
        pool.execute("UPDATE snapshot_probe SET value=2")
            .await
            .unwrap();
        let after: i32 = sqlx::query_scalar("SELECT value FROM snapshot_probe")
            .fetch_one(&mut *read)
            .await
            .unwrap();
        assert_eq!((before, after), (1, 1));
        read.commit().await.unwrap();
    }
}
