use axum::Router;
use axum::routing::{get, post, put};

use crate::contexts::classification::public::CategoryCatalogFacade;

use super::handlers;

pub(crate) fn router(categories: CategoryCatalogFacade) -> Router {
    Router::new()
        .route("/categories", post(handlers::create).get(handlers::list))
        .route(
            "/categories/{id}",
            get(handlers::get).patch(handlers::update),
        )
        .route("/categories/{id}/move", post(handlers::move_node))
        .route("/categories/reorder", put(handlers::reorder))
        .route("/categories/{id}/archive", post(handlers::archive))
        .route("/categories/{id}/restore", post(handlers::restore))
        .route("/category-icons", get(handlers::icons))
        .with_state(categories)
}
