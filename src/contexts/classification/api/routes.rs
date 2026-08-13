use axum::Router;
use axum::routing::{get, post};

use crate::contexts::classification::public::CategoryCatalogFacade;

use super::handlers;

pub(crate) fn router(categories: CategoryCatalogFacade) -> Router {
    Router::new()
        .route("/categories", post(handlers::create).get(handlers::list))
        .route(
            "/categories/{id}",
            get(handlers::get).patch(handlers::rename),
        )
        .route("/categories/{id}/archive", post(handlers::archive))
        .route("/categories/{id}/restore", post(handlers::restore))
        .with_state(categories)
}
