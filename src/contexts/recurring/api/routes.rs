use super::handlers;
use crate::contexts::recurring::public::RecurringFacade;
use axum::{
    Router,
    routing::{get, post},
};
pub(crate) fn router(f: RecurringFacade) -> Router {
    Router::new()
        .route("/subscriptions", get(handlers::list))
        .route("/subscriptions/forecast", get(handlers::forecast))
        .route(
            "/subscriptions/{subscription_id}",
            get(handlers::get).patch(handlers::patch),
        )
        .route(
            "/subscriptions/{subscription_id}/charges",
            get(handlers::charges),
        )
        .route(
            "/subscription-charges/{charge_evidence_id}/matches",
            post(handlers::create_match),
        )
        .route(
            "/subscription-charges/{charge_evidence_id}/rejections",
            post(handlers::reject),
        )
        .route(
            "/subscription-charges/{charge_evidence_id}/matches/{match_id}/unmatches",
            post(handlers::unmatch),
        )
        .with_state(f)
}
