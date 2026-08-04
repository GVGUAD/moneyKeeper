use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;

use crate::domain::error::DomainError;
use crate::domain::subscription_error::SubscriptionError;

pub struct AppError(anyhow::Error);

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let err = self.0;
        if let Some(domain) = err.downcast_ref::<DomainError>() {
            let (status, msg) = match domain {
                DomainError::NotFound(m) => (StatusCode::NOT_FOUND, m.clone()),
                DomainError::Unauthorized => (StatusCode::UNAUTHORIZED, "unauthorized".to_string()),
                DomainError::Conflict(m) => (StatusCode::CONFLICT, m.clone()),
                DomainError::InvalidInput(m) => (StatusCode::BAD_REQUEST, m.clone()),
            };
            return (status, Json(json!({"error": msg}))).into_response();
        }
        if let Some(s) = err.downcast_ref::<SubscriptionError>() {
            let (status, msg) = match s {
                SubscriptionError::ConnectionNotFound
                | SubscriptionError::SubscriptionNotFound
                | SubscriptionError::ChargeNotFound => (StatusCode::NOT_FOUND, s.to_string()),
                SubscriptionError::DuplicateCharge(_) => (StatusCode::CONFLICT, s.to_string()),
                _ => (StatusCode::INTERNAL_SERVER_ERROR, s.to_string()),
            };
            return (status, Json(json!({"error": msg}))).into_response();
        }
        tracing::error!("internal error: {err:?}");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": "internal server error"})),
        )
            .into_response()
    }
}

impl<E: Into<anyhow::Error>> From<E> for AppError {
    fn from(e: E) -> Self {
        Self(e.into())
    }
}
