use axum::{
    Router,
    body::Bytes,
    extract::{Path, State},
    http::StatusCode,
    routing::{get, post},
};

use super::handlers;
use crate::contexts::banking::public::BankingFacade;

pub(crate) fn webhook_router(banking: BankingFacade) -> Router {
    Router::new()
        .route(
            "/webhooks/monobank/{webhook_credential}",
            get(validate).post(receive),
        )
        .with_state(banking)
}

pub(crate) fn authenticated_router(banking: BankingFacade) -> Router {
    Router::new()
        .route("/provider-connections/monobank", post(handlers::connect))
        .route("/provider-connections", get(handlers::list_connections))
        .route("/provider-connections/{id}", get(handlers::get_connection))
        .route(
            "/provider-connections/{id}/disconnect",
            post(handlers::disconnect),
        )
        .route(
            "/provider-connections/{id}/credential-replacements",
            post(handlers::replace_credential),
        )
        .route(
            "/provider-connections/{id}/webhook-rotations",
            post(handlers::rotate_webhook),
        )
        .route(
            "/provider-connections/{id}/resources",
            get(handlers::list_resources),
        )
        .route(
            "/provider-connections/{id}/resource-mappings",
            post(handlers::map_resource),
        )
        .route(
            "/provider-connections/{id}/resource-mappings/{mapping_id}/deactivations",
            post(handlers::deactivate_mapping),
        )
        .route(
            "/provider-connections/{id}/resource-mappings/{mapping_id}/replacements",
            post(handlers::replace_mapping),
        )
        .route(
            "/provider-connections/{id}/sync-jobs",
            post(handlers::request_sync),
        )
        .route("/sync-jobs/{id}", get(handlers::get_sync))
        .route("/provider-events/{id}", get(handlers::get_event))
        .route("/accounting-processes/{id}", get(handlers::get_process))
        .route("/balance-observations/{id}", get(handlers::get_observation))
        .with_state(banking)
}

async fn validate(
    State(banking): State<BankingFacade>,
    Path(credential): Path<String>,
) -> StatusCode {
    match banking.validate_webhook_credential(&credential).await {
        Ok(true) => StatusCode::OK,
        _ => StatusCode::NOT_FOUND,
    }
}
async fn receive(
    State(banking): State<BankingFacade>,
    Path(credential): Path<String>,
    body: Bytes,
) -> StatusCode {
    match banking.receive_webhook(&credential, &body).await {
        Ok(_) => StatusCode::OK,
        _ => StatusCode::NOT_FOUND,
    }
}
