use axum::Router;
use axum::routing::get;

use crate::contexts::reference_data::public::CurrencyCatalogFacade;

use super::handlers;

pub(crate) fn router(catalog: CurrencyCatalogFacade) -> Router {
    Router::new()
        .route("/currencies", get(handlers::list))
        .route("/currencies/{code}", get(handlers::get))
        .with_state(catalog)
}
