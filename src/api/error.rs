use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;

use crate::domain::error::DomainError;
use crate::domain::subscription_error::SubscriptionError;
use crate::infrastructure::email::oauth::{GmailProviderError, OAuthFlowError};

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
                SubscriptionError::DuplicateCharge(_) | SubscriptionError::SyncInProgress => {
                    (StatusCode::CONFLICT, s.to_string())
                }
                _ => (StatusCode::INTERNAL_SERVER_ERROR, s.to_string()),
            };
            return (status, Json(json!({"error": msg}))).into_response();
        }
        if let Some(oauth) = err.downcast_ref::<OAuthFlowError>() {
            let status = match oauth {
                OAuthFlowError::InvalidState | OAuthFlowError::IncompleteRequest => {
                    StatusCode::BAD_REQUEST
                }
                OAuthFlowError::MissingRefreshToken
                | OAuthFlowError::InvalidProviderCredentials => StatusCode::BAD_GATEWAY,
            };
            return (status, Json(json!({"error": oauth.to_string()}))).into_response();
        }
        if let Some(provider) = err.downcast_ref::<GmailProviderError>() {
            let status = match provider {
                GmailProviderError::Transient => StatusCode::SERVICE_UNAVAILABLE,
                GmailProviderError::InvalidCredentials | GmailProviderError::Rejected => {
                    StatusCode::BAD_GATEWAY
                }
            };
            return (status, Json(json!({"error": provider.to_string()}))).into_response();
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gmail_provider_errors_have_retry_aware_statuses() {
        assert_eq!(
            AppError::from(GmailProviderError::Rejected)
                .into_response()
                .status(),
            StatusCode::BAD_GATEWAY
        );
        assert_eq!(
            AppError::from(GmailProviderError::Transient)
                .into_response()
                .status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
    }
}
