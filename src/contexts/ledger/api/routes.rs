use axum::Router;
use axum::routing::{get, post};

use crate::api::state::LedgerApiState;

use super::handlers;

pub(crate) fn router(state: LedgerApiState) -> Router {
    Router::new()
        .route(
            "/transfer-conversions/{id}/attachment-candidates",
            get(super::conversions::attachment_candidates),
        )
        .route(
            "/transfer-conversion-notifications",
            get(super::conversions::notifications),
        )
        .route(
            "/transfer-conversion-reviews",
            get(super::conversions::reviews),
        )
        .route(
            "/transfer-conversion-reviews/{id}",
            get(super::conversions::review),
        )
        .route(
            "/transfer-conversion-reviews/{id}/resolve",
            post(super::conversions::resolve_review),
        )
        .route(
            "/transfer-conversions/{id}/attachments",
            post(super::conversions::attach),
        )
        .route(
            "/transactions/{id}/transfer-candidates",
            get(super::conversions::candidates),
        )
        .route(
            "/transactions/{id}/transfer-conversion-preview",
            post(super::conversions::preview),
        )
        .route(
            "/transactions/{id}/transfer-conversions",
            post(super::conversions::convert),
        )
        .route(
            "/transfer-conversions/{id}",
            get(super::conversions::get).patch(super::conversions::edit),
        )
        .route(
            "/transfer-conversions/{id}/undo",
            post(super::conversions::undo),
        )
        .route(
            "/accounts",
            post(handlers::open_account).get(handlers::list_accounts),
        )
        .route(
            "/accounts/{id}",
            get(handlers::get_account).patch(handlers::rename_account),
        )
        .route("/accounts/{id}/archive", post(handlers::archive_account))
        .route("/accounts/{id}/restore", post(handlers::restore_account))
        .route("/accounts/{id}/activity", get(handlers::account_activity))
        .route(
            "/transactions",
            post(handlers::record_transaction).get(handlers::list_transactions),
        )
        .route(
            "/transactions/summary",
            get(handlers::summarize_transactions),
        )
        .route("/transactions/{id}", get(handlers::get_transaction))
        .route(
            "/transactions/{id}/annotation",
            axum::routing::patch(handlers::update_annotation),
        )
        .route(
            "/transactions/{id}/reversals",
            post(handlers::reverse_transaction),
        )
        .route(
            "/transactions/{id}/replacements",
            post(handlers::replace_transaction),
        )
        .route("/transfers", post(handlers::transfer))
        .route(
            "/accounts/{id}/balance-corrections",
            post(handlers::correct_balance),
        )
        .route("/reconciliations", get(handlers::list_reconciliations))
        .route("/reconciliations/{id}", get(handlers::get_reconciliation))
        .route(
            "/reconciliations/{id}/approve",
            post(handlers::approve_reconciliation),
        )
        .route(
            "/reconciliations/{id}/dismiss",
            post(handlers::dismiss_reconciliation),
        )
        .with_state(state)
}
