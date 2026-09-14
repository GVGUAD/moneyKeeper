use crate::api::{ApiError, AuthenticatedUser};
use crate::contexts::classification::public::CategoryId;
use crate::contexts::ledger::public::{ActivityCursor, ActivityKind};
use crate::contexts::reporting::public::*;
use crate::shared_kernel::CurrencyCode;
use axum::{
    Json, Router,
    extract::{Query, State, rejection::QueryRejection},
    routing::get,
};
use chrono::{DateTime, Utc};
use serde::Deserialize;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AggregateQuery {
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
    pub timezone: String,
    pub currency: CurrencyCode,
    pub category_id: Option<CategoryId>,
    pub category_scope: Option<CategoryScope>,
    #[serde(default)]
    pub uncategorized: bool,
    pub comparison_from: DateTime<Utc>,
    pub comparison_to: DateTime<Utc>,
    pub trend_months: Option<u32>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TransactionsQuery {
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
    pub timezone: String,
    pub currency: CurrencyCode,
    pub category_id: Option<CategoryId>,
    pub category_scope: Option<CategoryScope>,
    #[serde(default)]
    pub uncategorized: bool,
    pub kind: Option<ActivityKind>,
    pub limit: Option<u32>,
    pub after_occurred_at: Option<DateTime<Utc>>,
    pub after_sequence: Option<i64>,
}
fn error(e: AnalyticsError) -> ApiError {
    match e {
        AnalyticsError::Invalid(_) => ApiError::bad_request("invalid analytics parameters"),
        AnalyticsError::CategoryNotFound => ApiError::not_found("category not found"),
        AnalyticsError::TaxonomyChanged => {
            ApiError::conflict("category taxonomy changed; retry report")
        }
        AnalyticsError::Persistence => {
            ApiError::internal("reporting.persistence", "analytics query failed")
        }
    }
}
async fn aggregate(
    State(f): State<ReportingAnalyticsFacade>,
    AuthenticatedUser(user): AuthenticatedUser,
    q: Result<Query<AggregateQuery>, QueryRejection>,
) -> Result<Json<AnalyticsResponse>, ApiError> {
    let Query(q) = q.map_err(|_| ApiError::bad_request("invalid analytics parameters"))?;
    f.aggregate(
        user,
        AnalyticsRequest {
            selection: AnalyticsSelection {
                from: q.from,
                to: q.to,
                timezone: q.timezone,
                currency: q.currency,
                category_id: q.category_id,
                category_scope: q.category_scope,
                uncategorized: q.uncategorized,
            },
            comparison_from: q.comparison_from,
            comparison_to: q.comparison_to,
            trend_months: q.trend_months.unwrap_or(6),
        },
    )
    .await
    .map(Json)
    .map_err(error)
}
async fn transactions(
    State(f): State<ReportingAnalyticsFacade>,
    AuthenticatedUser(user): AuthenticatedUser,
    q: Result<Query<TransactionsQuery>, QueryRejection>,
) -> Result<Json<AnalyticsListResponse>, ApiError> {
    let Query(q) = q.map_err(|_| ApiError::bad_request("invalid analytics parameters"))?;
    let after = match (q.after_occurred_at, q.after_sequence) {
        (None, None) => None,
        (Some(occurred_at), Some(ledger_sequence)) => Some(ActivityCursor {
            occurred_at,
            ledger_sequence,
        }),
        _ => return Err(ApiError::bad_request("incomplete analytics cursor")),
    };
    f.transactions(
        user,
        AnalyticsListRequest {
            selection: AnalyticsSelection {
                from: q.from,
                to: q.to,
                timezone: q.timezone,
                currency: q.currency,
                category_id: q.category_id,
                category_scope: q.category_scope,
                uncategorized: q.uncategorized,
            },
            kind: q.kind.unwrap_or_default(),
            limit: q.limit.unwrap_or(50),
            after,
        },
    )
    .await
    .map(Json)
    .map_err(error)
}
pub(crate) fn router(f: ReportingAnalyticsFacade) -> Router {
    Router::new()
        .route("/reports/analytics", get(aggregate))
        .route("/reports/analytics/transactions", get(transactions))
        .with_state(f)
}
