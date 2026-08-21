use super::handlers;
use crate::contexts::mail::public::MailFacade;
use axum::{
    Router,
    routing::{get, post},
};
pub(crate) fn authenticated_router(f: MailFacade) -> Router {
    Router::new()
        .route(
            "/me/email-connections/gmail/oauth/start",
            post(handlers::oauth_start),
        )
        .route("/me/email-connections", get(handlers::list))
        .route(
            "/me/email-connections/{connection_id}/status",
            get(handlers::status),
        )
        .route(
            "/me/email-connections/{connection_id}/disconnect",
            post(handlers::disconnect),
        )
        .route(
            "/me/email-connections/{connection_id}/resync",
            post(handlers::resync),
        )
        .with_state(f)
}
pub(crate) fn callback_router(f: MailFacade) -> Router {
    Router::new()
        .route("/oauth/gmail/callback", get(handlers::callback))
        .with_state(f)
}
