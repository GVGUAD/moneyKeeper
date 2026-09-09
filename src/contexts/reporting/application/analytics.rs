//! Live report composition over public Ledger and Classification contracts.
use crate::contexts::classification::public::{
    CategoryCatalog, CategoryCatalogFacade, CategoryId, CategoryNodeView, CategoryView,
};
use crate::contexts::ledger::public::*;
use crate::contexts::reference_data::public::{CurrencyCatalog, CurrencyCatalogFacade};
use crate::shared_kernel::{CurrencyCode, UserId};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Clone)]
pub struct ReportingAnalyticsFacade {
    ledger: LedgerFacade,
    categories: CategoryCatalogFacade,
    currencies: CurrencyCatalogFacade,
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CategoryScope {
    Subtree,
    Direct,
}
#[derive(Clone, Debug)]
pub struct AnalyticsSelection {
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
    pub timezone: String,
    pub currency: CurrencyCode,
    pub category_id: Option<CategoryId>,
    pub category_scope: Option<CategoryScope>,
    pub uncategorized: bool,
}
#[derive(Clone, Debug)]
pub struct AnalyticsRequest {
    pub selection: AnalyticsSelection,
    pub comparison_from: DateTime<Utc>,
    pub comparison_to: DateTime<Utc>,
    pub trend_months: u32,
}
#[derive(Clone, Debug)]
pub struct AnalyticsListRequest {
    pub selection: AnalyticsSelection,
    pub kind: ActivityKind,
    pub limit: u32,
    pub after: Option<ActivityCursor>,
}
#[derive(Clone, Debug, Serialize)]
pub struct AnalyticsMetadata {
    pub as_of: DateTime<Utc>,
    pub currency: CurrencyCode,
    pub timezone: String,
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
    pub category_id: Option<CategoryId>,
    pub category_scope: Option<CategoryScope>,
    pub uncategorized: bool,
    pub taxonomy_version: i64,
    pub coverage: &'static str,
}
#[derive(Clone, Debug, Serialize)]
pub struct AggregateMetadata {
    #[serde(flatten)]
    pub selection: AnalyticsMetadata,
    pub comparison_from: DateTime<Utc>,
    pub comparison_to: DateTime<Utc>,
    pub series_granularity: String,
    pub trend_from: DateTime<Utc>,
    pub trend_to: DateTime<Utc>,
    pub trend_months: u32,
}
#[derive(Clone, Debug, Serialize)]
pub struct ListMetadata {
    #[serde(flatten)]
    pub selection: AnalyticsMetadata,
    pub kind: ActivityKind,
}
#[derive(Clone, Debug, Serialize)]
pub struct AnalyticsBreakdown {
    pub key: String,
    pub category_id: Option<CategoryId>,
    pub scope: &'static str,
    pub label: String,
    pub path: Vec<String>,
    pub color: String,
    pub icon: String,
    pub has_children: bool,
    pub current: AnalyticsTotals,
    pub comparison: AnalyticsTotals,
}
#[derive(Clone, Debug, Serialize)]
pub struct AnalyticsResponse {
    pub metadata: AggregateMetadata,
    pub current: AnalyticsTotals,
    pub comparison: AnalyticsTotals,
    pub breakdown: Vec<AnalyticsBreakdown>,
    pub series: Vec<AnalyticsBucket>,
    pub trend: Vec<AnalyticsBucket>,
}
#[derive(Clone, Debug, Serialize)]
pub struct AnalyticsListResponse {
    pub metadata: ListMetadata,
    pub summary: AnalyticsSummary,
    pub items: Vec<AnalyticsTransaction>,
    pub next_cursor: Option<ActivityCursor>,
}
#[derive(Debug, thiserror::Error)]
pub enum AnalyticsError {
    #[error("{0}")]
    Invalid(String),
    #[error("category not found")]
    CategoryNotFound,
    #[error("category taxonomy changed; retry report")]
    TaxonomyChanged,
    #[error("analytics query failed")]
    Persistence,
}
impl From<LedgerError> for AnalyticsError {
    fn from(e: LedgerError) -> Self {
        if e.is_invalid_state() {
            Self::Invalid(e.to_string())
        } else {
            Self::Persistence
        }
    }
}
fn flatten(nodes: &[CategoryNodeView], out: &mut HashMap<CategoryId, CategoryView>) {
    for n in nodes {
        out.insert(n.category.id, n.category.clone());
        flatten(&n.children, out);
    }
}
fn descends(
    id: CategoryId,
    ancestor: CategoryId,
    categories: &HashMap<CategoryId, CategoryView>,
) -> bool {
    let mut next = Some(id);
    while let Some(id) = next {
        if id == ancestor {
            return true;
        }
        next = categories.get(&id).and_then(|c| c.parent_id);
    }
    false
}
fn filter(
    s: &AnalyticsSelection,
    categories: &HashMap<CategoryId, CategoryView>,
) -> Result<AnalyticsFilter, AnalyticsError> {
    if s.uncategorized && (s.category_id.is_some() || s.category_scope.is_some())
        || s.category_scope.is_some() && s.category_id.is_none()
    {
        return Err(AnalyticsError::Invalid(
            "conflicting category filters".into(),
        ));
    }
    let selected = if let Some(id) = s.category_id {
        if !categories.contains_key(&id) {
            return Err(AnalyticsError::CategoryNotFound);
        }
        AnalyticsCategories::Assigned(
            categories
                .keys()
                .copied()
                .filter(|candidate| {
                    if s.category_scope == Some(CategoryScope::Direct) {
                        *candidate == id
                    } else {
                        descends(*candidate, id, categories)
                    }
                })
                .collect(),
        )
    } else if s.uncategorized {
        AnalyticsCategories::Uncategorized
    } else {
        AnalyticsCategories::All
    };
    Ok(AnalyticsFilter {
        currency: s.currency.clone(),
        categories: selected,
    })
}
fn metadata(s: &AnalyticsSelection, version: i64, as_of: DateTime<Utc>) -> AnalyticsMetadata {
    AnalyticsMetadata {
        as_of,
        currency: s.currency.clone(),
        timezone: s.timezone.clone(),
        from: s.from,
        to: s.to,
        category_id: s.category_id,
        category_scope: s
            .category_id
            .map(|_| s.category_scope.unwrap_or(CategoryScope::Subtree)),
        uncategorized: s.uncategorized,
        taxonomy_version: version,
        coverage: "recorded_transactions",
    }
}
impl ReportingAnalyticsFacade {
    pub fn new(
        ledger: LedgerFacade,
        categories: CategoryCatalogFacade,
        currencies: CurrencyCatalogFacade,
    ) -> Self {
        Self {
            ledger,
            categories,
            currencies,
        }
    }
    async fn validate_currency(&self, s: &AnalyticsSelection) -> Result<(), AnalyticsError> {
        let known = self
            .currencies
            .list_known()
            .await
            .map_err(|_| AnalyticsError::Persistence)?;
        if !known.iter().any(|c| c.code == s.currency) {
            return Err(AnalyticsError::Invalid("invalid currency".into()));
        }
        Ok(())
    }
    pub async fn aggregate(
        &self,
        user: UserId,
        request: AnalyticsRequest,
    ) -> Result<AnalyticsResponse, AnalyticsError> {
        self.validate_currency(&request.selection).await?;
        let s = &request.selection;
        retry_taxonomy_read(|| async {
            let as_of = Utc::now();
            let taxonomy = self
                .categories
                .taxonomy(user, as_of)
                .await
                .map_err(|_| AnalyticsError::Persistence)?;
            let mut categories = HashMap::new();
            flatten(&taxonomy.roots, &mut categories);
            let selected = filter(s, &categories)?;
            let mut calendar = self
                .ledger
                .analytics_calendar(AnalyticsCalendarRequest {
                    range: AnalyticsInterval {
                        from: s.from,
                        to: s.to,
                    },
                    comparison: Some(AnalyticsInterval {
                        from: request.comparison_from,
                        to: request.comparison_to,
                    }),
                    timezone: s.timezone.clone(),
                    trend_months: request.trend_months,
                })
                .await?;
            let mut intervals = vec![
                AnalyticsInterval {
                    from: s.from,
                    to: s.to,
                },
                AnalyticsInterval {
                    from: request.comparison_from,
                    to: request.comparison_to,
                },
            ];
            intervals.extend(calendar.series.iter().chain(&calendar.trend).map(|b| {
                AnalyticsInterval {
                    from: b.from,
                    to: b.to,
                }
            }));
            let facts = self
                .ledger
                .analytics_aggregate(user, selected, intervals)
                .await?;
            if facts.iter().any(|f| {
                f.category_id
                    .is_some_and(|id| !categories.contains_key(&id))
            }) {
                return Ok(None);
            }
            let mut current = AnalyticsTotals::default();
            let mut comparison = AnalyticsTotals::default();
            let mut breakdown = HashMap::<String, AnalyticsBreakdown>::new();
            for fact in facts {
                match fact.interval {
                    0 => current.add(&fact.totals),
                    1 => comparison.add(&fact.totals),
                    i if i < 2 + calendar.series.len() => {
                        calendar.series[i - 2].totals.add(&fact.totals)
                    }
                    i => calendar.trend[i - 2 - calendar.series.len()]
                        .totals
                        .add(&fact.totals),
                }
                if fact.interval > 1 || fact.totals.expense_count == 0 {
                    continue;
                }
                let row = breakdown_row(s, fact.category_id, &categories);
                let entry = breakdown.entry(row.key.clone()).or_insert(row);
                if fact.interval == 0 {
                    entry.current.add(&fact.expense_totals)
                } else {
                    entry.comparison.add(&fact.expense_totals)
                }
            }
            let version = self
                .categories
                .taxonomy(user, Utc::now())
                .await
                .map_err(|_| AnalyticsError::Persistence)?
                .version;
            if version != taxonomy.version {
                return Ok(None);
            }
            let mut breakdown: Vec<_> = breakdown.into_values().collect();
            breakdown.sort_by(|a, b| {
                b.current
                    .purchases
                    .cmp(&a.current.purchases)
                    .then(a.label.cmp(&b.label))
                    .then(a.key.cmp(&b.key))
            });
            Ok(Some(AnalyticsResponse {
                metadata: AggregateMetadata {
                    selection: metadata(s, version, as_of),
                    comparison_from: request.comparison_from,
                    comparison_to: request.comparison_to,
                    series_granularity: calendar.granularity,
                    trend_from: calendar.trend[0].from,
                    trend_to: s.to,
                    trend_months: request.trend_months,
                },
                current,
                comparison,
                breakdown,
                series: calendar.series,
                trend: calendar.trend,
            }))
        })
        .await
    }
    pub async fn transactions(
        &self,
        user: UserId,
        request: AnalyticsListRequest,
    ) -> Result<AnalyticsListResponse, AnalyticsError> {
        self.validate_currency(&request.selection).await?;
        let s = &request.selection;
        retry_taxonomy_read(|| async {
            let as_of = Utc::now();
            let taxonomy = self
                .categories
                .taxonomy(user, as_of)
                .await
                .map_err(|_| AnalyticsError::Persistence)?;
            let mut categories = HashMap::new();
            flatten(&taxonomy.roots, &mut categories);
            let selected = filter(s, &categories)?;
            self.ledger
                .analytics_calendar(AnalyticsCalendarRequest {
                    range: AnalyticsInterval {
                        from: s.from,
                        to: s.to,
                    },
                    comparison: None,
                    timezone: s.timezone.clone(),
                    trend_months: 6,
                })
                .await?;
            let page = self
                .ledger
                .analytics_transactions(
                    user,
                    AnalyticsTransactionsQuery {
                        filter: selected,
                        range: AnalyticsInterval {
                            from: s.from,
                            to: s.to,
                        },
                        kind: request.kind,
                        after: request.after,
                        limit: request.limit,
                    },
                )
                .await?;
            if page
                .assigned_category_ids
                .iter()
                .any(|id| !categories.contains_key(id))
            {
                return Ok(None);
            }
            let version = self
                .categories
                .taxonomy(user, Utc::now())
                .await
                .map_err(|_| AnalyticsError::Persistence)?
                .version;
            if version != taxonomy.version {
                return Ok(None);
            }
            Ok(Some(AnalyticsListResponse {
                metadata: ListMetadata {
                    selection: metadata(s, version, as_of),
                    kind: request.kind,
                },
                summary: page.summary,
                items: page.items,
                next_cursor: page.next_cursor,
            }))
        })
        .await
    }
}
fn breakdown_row(
    s: &AnalyticsSelection,
    id: Option<CategoryId>,
    categories: &HashMap<CategoryId, CategoryView>,
) -> AnalyticsBreakdown {
    let Some(mut id) = id else {
        return AnalyticsBreakdown {
            key: "uncategorized".into(),
            category_id: None,
            scope: "uncategorized",
            label: "Uncategorized".into(),
            path: vec![],
            color: "#808080".into(),
            icon: "tag".into(),
            has_children: false,
            current: AnalyticsTotals::default(),
            comparison: AnalyticsTotals::default(),
        };
    };
    let direct = Some(id) == s.category_id;
    if !direct {
        while let Some(parent) = categories[&id].parent_id {
            if Some(parent) == s.category_id {
                break;
            }
            id = parent;
        }
    }
    let category = &categories[&id];
    let scope = if direct { "direct" } else { "subtree" };
    let has_children = categories.values().any(|c| c.parent_id == Some(id));
    AnalyticsBreakdown {
        key: format!("{scope}:{id}"),
        category_id: Some(id),
        scope,
        label: if direct && has_children {
            format!("Directly in {}", category.name)
        } else {
            category.name.clone()
        },
        path: category.path.clone(),
        color: category.effective_color.clone(),
        icon: category.effective_icon.clone(),
        has_children,
        current: AnalyticsTotals::default(),
        comparison: AnalyticsTotals::default(),
    }
}

/// None means the complete composition must be discarded and read again.
async fn retry_taxonomy_read<T, F, Fut>(mut read: F) -> Result<T, AnalyticsError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<Option<T>, AnalyticsError>>,
{
    for _ in 0..2 {
        if let Some(result) = read().await? {
            return Ok(result);
        }
    }
    Err(AnalyticsError::TaxonomyChanged)
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    #[tokio::test]
    async fn taxonomy_retry_discards_first_composition_and_retries_only_once() {
        let reads = AtomicUsize::new(0);
        let result = retry_taxonomy_read(|| async {
            let attempt = reads.fetch_add(1, Ordering::SeqCst);
            Ok(if attempt == 0 {
                None
            } else {
                Some("fresh taxonomy and facts")
            })
        })
        .await
        .unwrap();
        assert_eq!(result, "fresh taxonomy and facts");
        assert_eq!(reads.load(Ordering::SeqCst), 2);
        let reads = AtomicUsize::new(0);
        let result: Result<(), _> = retry_taxonomy_read(|| async {
            reads.fetch_add(1, Ordering::SeqCst);
            Ok(None)
        })
        .await;
        assert!(matches!(result, Err(AnalyticsError::TaxonomyChanged)));
        assert_eq!(reads.load(Ordering::SeqCst), 2);
    }
}
