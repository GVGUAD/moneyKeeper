use super::handlers;
use crate::contexts::reporting::public::ReportingFacade;
use axum::{Router, routing::get};
pub(crate) fn router(f: ReportingFacade) -> Router {
    Router::new()
        .route("/reports/balance-history", get(handlers::balance_history))
        .route("/reports/cashflow", get(handlers::cashflow))
        .route("/reports/spending", get(handlers::spending))
        .route("/reports/liabilities", get(handlers::liabilities))
        .route("/reports/reconciliations", get(handlers::reconciliations))
        .route("/reports/recurring", get(handlers::recurring))
        .route("/reports/net-worth", get(handlers::net_worth))
        .with_state(f)
}
