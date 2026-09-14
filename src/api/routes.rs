//! Default unversioned Moneykeeper HTTP router and shared adapters.

use std::sync::Arc;

use axum::extract::{FromRequest, FromRequestParts, Request, State};
use axum::http::{Method, StatusCode, header::AUTHORIZATION, request::Parts};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::{Json, Router};
use jsonwebtoken::jwk::JwkSet;
use serde_json::json;
use uuid::Uuid;

use crate::api::jwt::verify_token;
use crate::api::middleware::AuthUser;
use crate::bootstrap::ContextFacades;
use crate::shared_kernel::UserId;

/// Composes all supporting and core context routes.
pub fn router(contexts: ContextFacades, jwks: Arc<JwkSet>) -> Router {
    let banking = contexts.banking.clone();
    let mail = contexts.mail.clone();
    let categories = contexts.categories.clone();
    let classification = contexts.classification.clone();
    let ledger = contexts.ledger.clone();
    let analytics = crate::contexts::reporting::public::ReportingAnalyticsFacade::new(
        ledger.clone(),
        categories.clone(),
        contexts.currencies.clone(),
    );
    let authenticated = Router::new()
        .merge(crate::contexts::reporting::api::analytics::router(
            analytics,
        ))
        .merge(crate::contexts::portfolio::api::routes::router(
            contexts.portfolio,
        ))
        .merge(crate::contexts::sharing::api::routes::router(
            contexts.sharing,
            contexts.currencies.clone(),
        ))
        .merge(crate::contexts::ledger::api::routes::router(
            crate::api::state::LedgerApiState {
                ledger: ledger.clone(),
                currencies: contexts.currencies.clone(),
                banking: Some(banking.clone()),
                categories: categories.clone(),
                classification: classification.clone(),
            },
        ))
        .merge(crate::contexts::banking::api::routes::authenticated_router(
            banking.clone(),
        ))
        .merge(crate::contexts::reference_data::api::routes::router(
            contexts.currencies.clone(),
        ))
        .merge(crate::contexts::mail::api::routes::authenticated_router(
            mail.clone(),
        ))
        .merge(crate::contexts::recurring::api::routes::router(
            contexts.recurring,
        ))
        .merge(crate::contexts::reporting::api::routes::router(
            contexts.reporting,
        ))
        .merge(crate::contexts::loans::api::routes::router(
            contexts.loans,
            contexts.currencies.clone(),
        ))
        .merge(crate::contexts::classification::api::routes::router(
            contexts.categories,
        ))
        .merge(crate::contexts::classification::api::automation::router(
            crate::contexts::classification::api::automation::ClassificationApiState {
                automation: classification,
                categories,
                ledger,
            },
        ))
        .merge(crate::contexts::preferences::api::routes::router(
            contexts.preferences,
            contexts.currencies,
        ))
        .layer(middleware::from_fn_with_state(
            AuthState { jwks },
            authenticate,
        ));
    crate::contexts::mail::api::routes::callback_router(mail)
        .merge(crate::contexts::banking::webhook_router(banking))
        .merge(authenticated)
        .layer(middleware::from_fn(reject_removed_legacy_mutations))
}

async fn reject_removed_legacy_mutations(request: Request, next: Next) -> Response {
    let path = request.uri().path().trim_matches('/');
    let segments: Vec<&str> = path.split('/').collect();
    let removed_hard_delete = request.method() == Method::DELETE
        && segments.len() == 2
        && matches!(segments.first().copied(), Some("accounts" | "transactions"));
    let removed_balance_setter = request.method() == Method::PATCH
        && segments.len() == 3
        && segments.first().copied() == Some("accounts")
        && segments.last().copied() == Some("balance");
    let missing_webhook_credential = path == "webhooks/monobank";
    let versioned_alias = path == "v2" || path.starts_with("v2/");
    if removed_hard_delete
        || removed_balance_setter
        || missing_webhook_credential
        || versioned_alias
    {
        return (StatusCode::NOT_FOUND, Json(json!({"error": "not_found"}))).into_response();
    }
    next.run(request).await
}

#[derive(Clone)]
struct AuthState {
    jwks: Arc<JwkSet>,
}

async fn authenticate(
    State(state): State<AuthState>,
    mut request: Request,
    next: Next,
) -> Result<Response, ApiError> {
    let token = request
        .headers()
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .ok_or_else(ApiError::unauthorized)?;
    let claims = verify_token(token, &state.jwks).map_err(|_| ApiError::unauthorized())?;
    let user_id = Uuid::parse_str(&claims.sub).map_err(|_| ApiError::unauthorized())?;
    request.extensions_mut().insert(AuthUser(user_id));
    Ok(next.run(request).await)
}

/// The exact Moneykeeper method/path manifest used to validate OpenAPI parity.
pub const ROUTE_MANIFEST: &[(&str, &str)] = &[
    ("POST", "/portfolio-accounts"),
    ("GET", "/portfolio-accounts"),
    ("GET", "/portfolio-accounts/{id}"),
    ("PATCH", "/portfolio-accounts/{id}"),
    ("POST", "/portfolio-accounts/{id}/archive"),
    ("POST", "/portfolio-accounts/{id}/restore"),
    ("GET", "/portfolio-accounts/{id}/activity"),
    ("POST", "/instruments/ovdp"),
    ("GET", "/instruments"),
    ("GET", "/instruments/{id}"),
    ("POST", "/portfolio-transactions"),
    ("POST", "/portfolio-transactions/{id}/reversals"),
    ("GET", "/portfolio-positions"),
    ("POST", "/valuations"),
    ("GET", "/valuations"),
    ("GET", "/currencies"),
    ("GET", "/currencies/{code}"),
    ("POST", "/categories"),
    ("GET", "/categories"),
    ("GET", "/categories/{id}"),
    ("PATCH", "/categories/{id}"),
    ("POST", "/categories/{id}/move"),
    ("PUT", "/categories/reorder"),
    ("POST", "/categories/{id}/archive"),
    ("POST", "/categories/{id}/restore"),
    ("GET", "/category-icons"),
    ("GET", "/preferences"),
    ("PATCH", "/preferences"),
    ("POST", "/accounts"),
    ("GET", "/accounts"),
    ("GET", "/accounts/{id}"),
    ("PATCH", "/accounts/{id}"),
    ("POST", "/accounts/{id}/archive"),
    ("POST", "/accounts/{id}/restore"),
    ("GET", "/accounts/{id}/activity"),
    ("POST", "/transactions"),
    ("GET", "/transactions"),
    ("GET", "/transactions/summary"),
    ("GET", "/transactions/{id}"),
    ("PATCH", "/transactions/{id}/annotation"),
    ("POST", "/transactions/{id}/classification/retry"),
    ("POST", "/transactions/{id}/reversals"),
    ("POST", "/transactions/{id}/replacements"),
    ("GET", "/transactions/{id}/transfer-candidates"),
    ("POST", "/transactions/{id}/transfer-conversion-preview"),
    ("POST", "/transactions/{id}/transfer-conversions"),
    ("GET", "/transfer-conversions/{id}"),
    ("PATCH", "/transfer-conversions/{id}"),
    ("POST", "/transfer-conversions/{id}/undo"),
    ("POST", "/transfer-conversions/{id}/attachments"),
    ("GET", "/transfer-conversion-reviews"),
    ("GET", "/transfer-conversion-reviews/{id}"),
    ("POST", "/transfer-conversion-reviews/{id}/resolve"),
    ("GET", "/transfer-conversion-notifications"),
    ("GET", "/transfer-conversions/{id}/attachment-candidates"),
    ("POST", "/transfers"),
    ("POST", "/accounts/{id}/balance-corrections"),
    ("GET", "/classification/review-queue"),
    ("POST", "/classification/decisions/{id}/resolve"),
    ("POST", "/classification/backfills"),
    ("GET", "/classification/backfills/{id}"),
    ("GET", "/reconciliations"),
    ("GET", "/reconciliations/{id}"),
    ("POST", "/reconciliations/{id}/approve"),
    ("POST", "/reconciliations/{id}/dismiss"),
    ("POST", "/provider-connections/monobank"),
    ("GET", "/provider-connections"),
    ("GET", "/provider-connections/{id}"),
    ("POST", "/provider-connections/{id}/disconnect"),
    ("POST", "/provider-connections/{id}/credential-replacements"),
    ("POST", "/provider-connections/{id}/webhook-rotations"),
    ("GET", "/provider-connections/{id}/resources"),
    ("POST", "/provider-connections/{id}/resource-mappings"),
    (
        "POST",
        "/provider-connections/{id}/resource-mappings/{mapping_id}/deactivations",
    ),
    (
        "POST",
        "/provider-connections/{id}/resource-mappings/{mapping_id}/replacements",
    ),
    ("POST", "/provider-connections/{id}/sync-jobs"),
    ("GET", "/provider-connections/{id}/provider-event-conflicts"),
    ("GET", "/sync-jobs/{id}"),
    ("GET", "/sync-jobs/{id}/pages"),
    ("GET", "/provider-events/{id}"),
    ("GET", "/accounting-processes/{id}"),
    ("GET", "/balance-observations/{id}"),
    ("POST", "/me/email-connections/gmail/oauth/start"),
    ("GET", "/oauth/gmail/callback"),
    ("GET", "/me/email-connections"),
    ("GET", "/me/email-connections/{connection_id}/status"),
    ("POST", "/me/email-connections/{connection_id}/disconnect"),
    ("POST", "/me/email-connections/{connection_id}/resync"),
    ("GET", "/subscriptions"),
    ("GET", "/subscriptions/{subscription_id}"),
    ("PATCH", "/subscriptions/{subscription_id}"),
    ("GET", "/subscriptions/{subscription_id}/charges"),
    ("GET", "/subscriptions/forecast"),
    ("POST", "/subscription-charges/{charge_evidence_id}/matches"),
    (
        "POST",
        "/subscription-charges/{charge_evidence_id}/rejections",
    ),
    (
        "POST",
        "/subscription-charges/{charge_evidence_id}/matches/{match_id}/unmatches",
    ),
    ("GET", "/fx-rates"),
    ("GET", "/reports/analytics"),
    ("GET", "/reports/analytics/transactions"),
    ("GET", "/reports/balance-history"),
    ("GET", "/reports/cashflow"),
    ("GET", "/reports/spending"),
    ("GET", "/reports/liabilities"),
    ("GET", "/reports/reconciliations"),
    ("GET", "/reports/recurring"),
    ("GET", "/reports/net-worth"),
    ("GET", "/loans"),
    ("GET", "/loans/{id}"),
    ("GET", "/loans/{id}/term-revisions"),
    ("GET", "/loans/{id}/movements"),
    ("GET", "/loans/{id}/movements/{movement_id}"),
    ("POST", "/loans"),
    ("POST", "/loans/{id}/term-revisions"),
    ("POST", "/loans/{id}/closure"),
    ("POST", "/loans/{id}/disbursements"),
    ("POST", "/loans/{id}/repayments"),
    ("POST", "/loans/{id}/interest-accruals"),
    ("POST", "/loans/{id}/write-offs"),
    ("POST", "/loans/{id}/movements/{movement_id}/reversals"),
    ("POST", "/loans/{id}/movements/{movement_id}/replacements"),
    ("POST", "/contacts"),
    ("GET", "/contacts"),
    ("GET", "/contacts/{id}"),
    ("PATCH", "/contacts/{id}"),
    ("POST", "/contacts/{id}/archive"),
    ("POST", "/bill-splits"),
    ("GET", "/bill-splits"),
    ("GET", "/bill-splits/{id}"),
    ("POST", "/bill-splits/{id}/revisions"),
    ("POST", "/bill-splits/{id}/settlements"),
    ("GET", "/bill-splits/{id}/settlements"),
    (
        "POST",
        "/bill-splits/{id}/settlements/{settlement_id}/reversal",
    ),
    ("POST", "/bill-splits/{id}/cancellations"),
];

/// Authenticated tenant identity extracted from the existing auth boundary.
pub struct AuthenticatedUser(pub UserId);

impl<S> FromRequestParts<S> for AuthenticatedUser
where
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        parts
            .extensions
            .get::<AuthUser>()
            .map(|user| Self(UserId::new(user.0)))
            .ok_or_else(ApiError::unauthorized)
    }
}

/// JSON extractor that keeps every request failure on the stable JSON error
/// contract instead of leaking Axum's plain-text rejection responses.
pub struct ApiJson<T>(pub T);

impl<S, T> FromRequest<S> for ApiJson<T>
where
    S: Send + Sync,
    Json<T>: FromRequest<S>,
{
    type Rejection = ApiError;

    async fn from_request(request: Request, state: &S) -> Result<Self, Self::Rejection> {
        Json::<T>::from_request(request, state)
            .await
            .map(|Json(value)| Self(value))
            .map_err(|_| ApiError::bad_request("invalid JSON request"))
    }
}

/// Stable HTTP error translation for the Moneykeeper API.
#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    message: &'static str,
    diagnostic: Option<ApiDiagnostic>,
}

#[derive(Debug)]
struct ApiDiagnostic {
    category: &'static str,
    message: &'static str,
}

impl ApiError {
    pub fn bad_request(message: &'static str) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message,
            diagnostic: None,
        }
    }

    pub fn unauthorized() -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            message: "unauthorized",
            diagnostic: None,
        }
    }

    pub fn not_found(message: &'static str) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            message,
            diagnostic: None,
        }
    }

    pub fn conflict(message: &'static str) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            message,
            diagnostic: None,
        }
    }

    pub fn bad_gateway(
        message: &'static str,
        category: &'static str,
        diagnostic_message: &'static str,
    ) -> Self {
        Self {
            status: StatusCode::BAD_GATEWAY,
            message,
            diagnostic: Some(ApiDiagnostic {
                category,
                message: diagnostic_message,
            }),
        }
    }

    pub fn internal(category: &'static str, diagnostic_message: &'static str) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: "internal server error",
            diagnostic: Some(ApiDiagnostic {
                category,
                message: diagnostic_message,
            }),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        if let Some(diagnostic) = &self.diagnostic {
            tracing::error!(
                event.name = "api.error",
                http.status = self.status.as_u16(),
                error.category = diagnostic.category,
                error.message = diagnostic.message,
                "API request failed"
            );
        }
        (self.status, Json(json!({"error": self.message}))).into_response()
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::sync::{Arc, Mutex};

    use axum::body::to_bytes;
    use tracing_subscriber::fmt::MakeWriter;

    use super::{ApiError, IntoResponse, StatusCode};

    #[derive(Clone, Default)]
    struct Buffer(Arc<Mutex<Vec<u8>>>);

    struct BufferWriter(Arc<Mutex<Vec<u8>>>);

    impl Write for BufferWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'writer> MakeWriter<'writer> for Buffer {
        type Writer = BufferWriter;

        fn make_writer(&'writer self) -> Self::Writer {
            BufferWriter(Arc::clone(&self.0))
        }
    }

    #[tokio::test]
    async fn internal_errors_keep_the_response_stable_and_drop_source_details() {
        let source = anyhow::anyhow!("bound-value-sentinel");
        let error = Err::<(), _>(source)
            .map_err(|_| ApiError::internal("ledger.persistence", "ledger request failed"))
            .unwrap_err();
        let output = Buffer::default();
        let subscriber = tracing_subscriber::fmt()
            .json()
            .flatten_event(true)
            .with_ansi(false)
            .with_writer(output.clone())
            .finish();

        let response = tracing::subscriber::with_default(subscriber, || error.into_response());
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&body).unwrap(),
            serde_json::json!({"error": "internal server error"})
        );

        let logs = String::from_utf8(output.0.lock().unwrap().clone()).unwrap();
        assert!(logs.contains("api.error"));
        assert!(logs.contains("ledger.persistence"));
        assert!(logs.contains("ledger request failed"));
        assert!(!logs.contains("bound-value-sentinel"));
    }
}
