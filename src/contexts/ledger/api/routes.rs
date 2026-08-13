use axum::Router;
use axum::routing::{get, post};

use crate::api::v2_state::LedgerApiState;

use super::handlers;

pub(crate) fn router(state: LedgerApiState) -> Router {
    Router::new()
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
        .with_state(state)
}
