//! Isolated V2 Loans routes.

use super::handlers;
use crate::contexts::loans::public::LoansFacade;
use crate::contexts::reference_data::public::CurrencyCatalogFacade;
use axum::{
    Router,
    routing::{get, post},
};

#[derive(Clone)]
pub(crate) struct LoansApiState {
    pub(crate) loans: LoansFacade,
    pub(crate) currencies: CurrencyCatalogFacade,
}
pub(crate) fn router(loans: LoansFacade, currencies: CurrencyCatalogFacade) -> Router {
    Router::new()
        .route("/loans", get(handlers::list).post(handlers::open))
        .route("/loans/{id}", get(handlers::get))
        .route(
            "/loans/{id}/term-revisions",
            get(handlers::terms).post(handlers::revise),
        )
        .route("/loans/{id}/movements", get(handlers::movements))
        .route(
            "/loans/{id}/movements/{movement_id}",
            get(handlers::movement),
        )
        .route("/loans/{id}/closure", post(handlers::close))
        .route("/loans/{id}/disbursements", post(handlers::disburse))
        .route("/loans/{id}/repayments", post(handlers::repay))
        .route("/loans/{id}/interest-accruals", post(handlers::accrue))
        .route("/loans/{id}/write-offs", post(handlers::write_off))
        .route(
            "/loans/{id}/movements/{movement_id}/reversals",
            post(handlers::reverse),
        )
        .route(
            "/loans/{id}/movements/{movement_id}/replacements",
            post(handlers::replace),
        )
        .with_state(LoansApiState { loans, currencies })
}
