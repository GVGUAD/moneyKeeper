mod analytics_support;
use analytics_support::*;
use moneykeeper::contexts::ledger::public::*;
use rust_decimal::Decimal;
#[tokio::test]
async fn accounting_fixture_and_pages_reconcile() {
    let f = Fixture::new().await;
    f.acceptance().await;
    let rows = f
        .contexts
        .ledger
        .analytics_aggregate(f.user, filter(), vec![range(), range()])
        .await
        .unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].totals, rows[1].totals);
    let t = &rows[0].totals;
    assert_eq!(
        (t.income, t.expenses, t.net, t.purchases, t.expense_credits),
        (
            Decimal::from(1200),
            Decimal::from(145),
            Decimal::from(1055),
            Decimal::from(165),
            Decimal::from(20)
        )
    );
    assert_eq!(
        (t.income_count, t.expense_count, t.transaction_count),
        (1, 4, 5)
    );
    for (kind, count, total) in [
        (ActivityKind::All, 5, 1055),
        (ActivityKind::Income, 1, 1200),
        (ActivityKind::Expense, 4, -145),
    ] {
        let mut after = None;
        let mut ids = std::collections::HashSet::new();
        let mut sum = Decimal::ZERO;
        loop {
            let page = f
                .contexts
                .ledger
                .analytics_transactions(
                    f.user,
                    AnalyticsTransactionsQuery {
                        filter: filter(),
                        range: range(),
                        kind,
                        limit: 2,
                        after,
                    },
                )
                .await
                .unwrap();
            assert_eq!(page.summary.transaction_count, count);
            assert_eq!(page.summary.contribution, Decimal::from(total));
            for item in page.items {
                assert!(ids.insert(item.journal_entry_id));
                sum += item.contribution;
            }
            after = page.next_cursor;
            if after.is_none() {
                break;
            }
        }
        assert_eq!(ids.len() as i64, count);
        assert_eq!(sum, Decimal::from(total));
    }
    let mut empty = filter();
    empty.categories = AnalyticsCategories::Assigned(vec![]);
    assert!(
        f.contexts
            .ledger
            .analytics_aggregate(f.user, empty, vec![range()])
            .await
            .unwrap()
            .is_empty()
    );
}
#[tokio::test]
async fn signed_components_mixed_journals_precision_and_repeated_corrections() {
    let f = Fixture::new().await;
    let mixed = f
        .journal("100", "100", "UAH", "2026-08-01T00:00:00Z", None)
        .await;
    f.journal(
        "-2.12345678",
        "-3.12345678",
        "UAH",
        "2026-08-02T00:00:00Z",
        None,
    )
    .await;
    let old = f
        .journal("0", "10", "UAH", "2026-08-03T00:00:00Z", None)
        .await;
    let next = f
        .journal("0", "20", "UAH", "2026-08-03T00:00:00Z", Some((old, false)))
        .await;
    f.journal(
        "0",
        "30",
        "UAH",
        "2026-08-03T00:00:00Z",
        Some((next, false)),
    )
    .await;
    let page = f
        .contexts
        .ledger
        .analytics_transactions(
            f.user,
            AnalyticsTransactionsQuery {
                filter: filter(),
                range: range(),
                kind: ActivityKind::All,
                limit: 200,
                after: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(page.summary.transaction_count, 3);
    assert_eq!(page.summary.contribution, Decimal::from(-29));
    assert_eq!(
        page.items
            .iter()
            .find(|i| i.journal_entry_id.into_uuid() == mixed)
            .unwrap()
            .net,
        Decimal::ZERO
    );
    let rows = f
        .contexts
        .ledger
        .analytics_aggregate(f.user, filter(), vec![range()])
        .await
        .unwrap();
    assert_eq!(
        rows[0].totals.income,
        "97.87654322".parse::<Decimal>().unwrap()
    );
}
#[tokio::test]
async fn summary_is_full_range_beyond_two_hundred_rows() {
    let f = Fixture::new().await;
    for _ in 0..205 {
        f.journal("0", "1", "UAH", "2026-08-10T12:00:00Z", None)
            .await;
    }
    let first = f
        .contexts
        .ledger
        .analytics_transactions(
            f.user,
            AnalyticsTransactionsQuery {
                filter: filter(),
                range: range(),
                kind: ActivityKind::Expense,
                limit: 200,
                after: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(first.items.len(), 200);
    assert_eq!(first.summary.transaction_count, 205);
    let next = f
        .contexts
        .ledger
        .analytics_transactions(
            f.user,
            AnalyticsTransactionsQuery {
                filter: filter(),
                range: range(),
                kind: ActivityKind::Expense,
                limit: 200,
                after: first.next_cursor,
            },
        )
        .await
        .unwrap();
    assert_eq!(next.items.len(), 5);
    assert!(next.next_cursor.is_none());
    assert_eq!(next.summary.transaction_count, 205);
}
#[tokio::test]
async fn calendar_thresholds_dst_and_trend_boundaries() {
    let f = Fixture::new().await;
    for (from, to, unit, count) in [
        ("2026-01-01T00:00:00Z", "2026-02-01T00:00:00Z", "day", 31),
        ("2026-01-01T00:00:00Z", "2026-02-02T00:00:00Z", "week", 5),
        ("2026-01-01T00:00:00Z", "2026-04-04T00:00:00Z", "week", 14),
        ("2026-01-01T00:00:00Z", "2026-04-05T00:00:00Z", "month", 4),
        ("2024-02-01T00:00:00Z", "2024-03-01T00:00:00Z", "day", 29),
    ] {
        let cal = f
            .contexts
            .ledger
            .analytics_calendar(AnalyticsCalendarRequest {
                range: AnalyticsInterval {
                    from: at(from),
                    to: at(to),
                },
                comparison: None,
                timezone: "UTC".into(),
                trend_months: 12,
            })
            .await
            .unwrap();
        assert_eq!(cal.granularity, unit);
        assert_eq!(cal.series.len(), count);
        assert_eq!(cal.trend.len(), 12);
        assert_eq!(cal.series.first().unwrap().from, at(from));
        assert_eq!(cal.series.last().unwrap().to, at(to));
    }
    for (from, to, hours) in [
        ("2026-03-28T22:00:00Z", "2026-03-29T21:00:00Z", 23),
        ("2026-10-24T21:00:00Z", "2026-10-25T22:00:00Z", 25),
    ] {
        let cal = f
            .contexts
            .ledger
            .analytics_calendar(AnalyticsCalendarRequest {
                range: AnalyticsInterval {
                    from: at(from),
                    to: at(to),
                },
                comparison: None,
                timezone: "Europe/Kyiv".into(),
                trend_months: 6,
            })
            .await
            .unwrap();
        assert_eq!(cal.series.len(), 1);
        assert!(!cal.series[0].partial);
        assert_eq!((cal.series[0].to - cal.series[0].from).num_hours(), hours);
        assert_eq!(cal.trend.len(), 6);
    }
}

#[tokio::test]
async fn refresh_restates_reports_and_lists_while_preserving_audit_entries() {
    let f = Fixture::new().await;
    let original = f
        .journal("0", "40", "UAH", "2026-08-10T12:00:00Z", None)
        .await;
    let before = f
        .contexts
        .ledger
        .analytics_aggregate(f.user, filter(), vec![range()])
        .await
        .unwrap();
    assert_eq!(before[0].totals.expenses, Decimal::from(40));
    f.journal(
        "0",
        "-40",
        "UAH",
        "2026-09-10T12:00:00Z",
        Some((original, true)),
    )
    .await;
    assert!(
        f.contexts
            .ledger
            .analytics_aggregate(f.user, filter(), vec![range()])
            .await
            .unwrap()
            .is_empty()
    );
    let page = f
        .contexts
        .ledger
        .analytics_transactions(
            f.user,
            AnalyticsTransactionsQuery {
                filter: filter(),
                range: range(),
                kind: ActivityKind::Expense,
                limit: 50,
                after: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(page.summary.transaction_count, 0);
    assert_eq!(page.summary.contribution, Decimal::ZERO);
    assert!(page.items.is_empty());
    let activity = f
        .contexts
        .ledger
        .list_journals(f.user, None, 50)
        .await
        .unwrap();
    assert_eq!(activity.len(), 2);
    assert!(activity.iter().any(|j| j.id.into_uuid() == original));
}
