use axum::Router;
use axum::http::StatusCode;
use axum::http::header;
use axum::middleware as axum_middleware;
use axum::response::{Html, IntoResponse};
use axum::routing::{delete, get, post, put};

use crate::api::handlers::{
    accounts, categories, email_connections, monobank, subscriptions, transactions, user_settings,
};
use crate::api::middleware::auth_middleware;
use crate::api::state::AppState;

const OPENAPI_SPEC: &str = include_str!("../../static/openapi.json");

async fn openapi_json() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "application/json")], OPENAPI_SPEC)
}

async fn swagger_ui() -> Html<&'static str> {
    Html(include_str!("../../static/swagger-ui.html"))
}

pub fn router(state: AppState) -> Router {
    let protected = Router::new()
        .route(
            "/accounts",
            post(accounts::create_account).get(accounts::list_accounts),
        )
        .route(
            "/accounts/{id}",
            get(accounts::get_account)
                .put(accounts::update_account)
                .delete(accounts::delete_account),
        )
        .route(
            "/accounts/{id}/transactions",
            post(transactions::create_transaction).get(transactions::list_transactions),
        )
        .route("/transactions", get(transactions::list_all_transactions))
        .route(
            "/transactions/{id}",
            get(transactions::get_transaction).delete(transactions::delete_transaction),
        )
        .route(
            "/categories",
            post(categories::create_category).get(categories::list_categories),
        )
        .route(
            "/categories/{id}",
            put(categories::update_category).delete(categories::delete_category),
        )
        .route("/monobank/client-info", get(monobank::get_client_info))
        .route("/monobank/connect", post(monobank::connect))
        .route("/monobank/connections", get(monobank::list_connections))
        .route(
            "/monobank/connections/{id}",
            delete(monobank::delete_connection),
        )
        .route(
            "/monobank/connections/{id}/resync",
            post(monobank::resync_connection),
        )
        .route(
            "/me/settings",
            get(user_settings::get_settings).patch(user_settings::update_settings),
        )
        .route(
            "/me/email-connections/gmail/oauth/start",
            post(email_connections::oauth_start),
        )
        .route(
            "/me/email-connections/gmail/oauth/callback",
            post(email_connections::oauth_callback),
        )
        .route("/me/email-connections", get(email_connections::list))
        .route(
            "/me/email-connections/{id}",
            delete(email_connections::delete),
        )
        .route(
            "/me/email-connections/{id}/resync",
            post(email_connections::resync),
        )
        .route("/subscriptions", get(subscriptions::list))
        .route("/subscriptions/forecast", get(subscriptions::forecast))
        .route(
            "/subscriptions/{id}",
            get(subscriptions::get)
                .patch(subscriptions::patch)
                .delete(subscriptions::delete),
        )
        .route(
            "/subscriptions/{id}/charges",
            get(subscriptions::list_charges),
        )
        .route(
            "/subscription-charges/{id}/link",
            post(subscriptions::link_charge),
        )
        .route(
            "/subscription-charges/{id}/unlink",
            post(subscriptions::unlink_charge),
        )
        .layer(axum_middleware::from_fn_with_state(
            state.clone(),
            auth_middleware,
        ));

    Router::new()
        .route("/api-doc/openapi.json", get(openapi_json))
        .route("/swagger-ui", get(swagger_ui))
        .route("/health", get(|| async { (StatusCode::OK, "ok") }))
        .merge(protected)
        .route("/monobank/webhook", post(monobank::webhook))
        .with_state(state)
}
