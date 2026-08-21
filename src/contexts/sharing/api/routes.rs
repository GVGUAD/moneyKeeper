//! Sharing isolated V2 routes.
use super::handlers;
use crate::contexts::reference_data::public::CurrencyCatalogFacade;
use crate::contexts::sharing::public::SharingFacade;
use axum::{
    Router,
    routing::{get, post},
};

#[derive(Clone)]
pub(crate) struct SharingApiState {
    pub sharing: SharingFacade,
    pub currencies: CurrencyCatalogFacade,
}

pub(crate) fn router(sharing: SharingFacade, currencies: CurrencyCatalogFacade) -> Router {
    Router::new()
        .route(
            "/contacts",
            post(handlers::create_contact).get(handlers::list_contacts),
        )
        .route(
            "/contacts/{id}",
            get(handlers::get_contact).patch(handlers::update_contact),
        )
        .route("/contacts/{id}/archive", post(handlers::archive_contact))
        .route(
            "/bill-splits",
            post(handlers::create_bill).get(handlers::list_bills),
        )
        .route("/bill-splits/{id}", get(handlers::get_bill))
        .route("/bill-splits/{id}/revisions", post(handlers::revise_bill))
        .route(
            "/bill-splits/{id}/settlements",
            post(handlers::create_settlement),
        )
        .route(
            "/bill-splits/{id}/settlements/{settlement_id}/reversal",
            post(handlers::reverse_settlement),
        )
        .route(
            "/bill-splits/{id}/cancellations",
            post(handlers::cancel_bill),
        )
        .with_state(SharingApiState {
            sharing,
            currencies,
        })
}
