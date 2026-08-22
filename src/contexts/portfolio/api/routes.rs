use super::handlers;
use crate::contexts::portfolio::public::PortfolioFacade;
use axum::{
    Router,
    routing::{get, post},
};
pub(crate) fn router(portfolio: PortfolioFacade) -> Router {
    Router::new()
        .route(
            "/portfolio-accounts",
            get(handlers::accounts).post(handlers::open_account),
        )
        .route(
            "/portfolio-accounts/{id}",
            get(handlers::account).patch(handlers::rename_account),
        )
        .route(
            "/portfolio-accounts/{id}/archive",
            post(handlers::archive_account),
        )
        .route(
            "/portfolio-accounts/{id}/restore",
            post(handlers::restore_account),
        )
        .route("/portfolio-accounts/{id}/activity", get(handlers::activity))
        .route("/instruments", get(handlers::instruments))
        .route("/instruments/{id}", get(handlers::instrument))
        .route("/instruments/ovdp", post(handlers::create_ovdp))
        .route(
            "/portfolio-transactions",
            post(handlers::record_transaction),
        )
        .route(
            "/portfolio-transactions/{id}/reversals",
            post(handlers::reverse_transaction),
        )
        .route("/portfolio-positions", get(handlers::positions))
        .route(
            "/valuations",
            get(handlers::valuations).post(handlers::record_valuation),
        )
        .with_state(portfolio)
}
