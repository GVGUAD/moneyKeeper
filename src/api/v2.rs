//! Isolated Finance V2 router composition.
//!
//! It is deliberately not merged into the legacy router during Phase 1.

use std::sync::Arc;

use axum::extract::{FromRequest, FromRequestParts, Request, State};
use axum::http::{StatusCode, header::AUTHORIZATION, request::Parts};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::{Json, Router};
use jsonwebtoken::jwk::JwkSet;
use serde_json::json;
use uuid::Uuid;

use crate::api::jwt::verify_token;
use crate::api::middleware::AuthUser;
use crate::bootstrap::v2::SupportingContexts;
use crate::shared_kernel::UserId;

/// Composes the parallel supporting-context routes from a verified V2 pool.
pub fn router(contexts: SupportingContexts, jwks: Arc<JwkSet>) -> Router {
    Router::new()
        .merge(crate::contexts::reference_data::api::routes::router(
            contexts.currencies.clone(),
        ))
        .merge(crate::contexts::classification::api::routes::router(
            contexts.categories,
        ))
        .merge(crate::contexts::preferences::api::routes::router(
            contexts.preferences,
            contexts.currencies,
        ))
        .layer(middleware::from_fn_with_state(
            V2AuthState { jwks },
            authenticate,
        ))
}

#[derive(Clone)]
struct V2AuthState {
    jwks: Arc<JwkSet>,
}

async fn authenticate(
    State(state): State<V2AuthState>,
    mut request: Request,
    next: Next,
) -> Result<Response, V2ApiError> {
    let token = request
        .headers()
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .ok_or_else(V2ApiError::unauthorized)?;
    let claims = verify_token(token, &state.jwks).map_err(|_| V2ApiError::unauthorized())?;
    let user_id = Uuid::parse_str(&claims.sub).map_err(|_| V2ApiError::unauthorized())?;
    request.extensions_mut().insert(AuthUser(user_id));
    Ok(next.run(request).await)
}

/// The exact Phase 1 method/path manifest used to validate OpenAPI parity.
pub const ROUTE_MANIFEST: &[(&str, &str)] = &[
    ("GET", "/currencies"),
    ("GET", "/currencies/{code}"),
    ("POST", "/categories"),
    ("GET", "/categories"),
    ("GET", "/categories/{id}"),
    ("PATCH", "/categories/{id}"),
    ("POST", "/categories/{id}/archive"),
    ("POST", "/categories/{id}/restore"),
    ("GET", "/preferences"),
    ("PATCH", "/preferences"),
];

/// Authenticated tenant identity extracted from the existing auth boundary.
pub(crate) struct AuthenticatedUser(pub UserId);

impl<S> FromRequestParts<S> for AuthenticatedUser
where
    S: Send + Sync,
{
    type Rejection = V2ApiError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        parts
            .extensions
            .get::<AuthUser>()
            .map(|user| Self(UserId::new(user.0)))
            .ok_or_else(V2ApiError::unauthorized)
    }
}

/// JSON extractor that keeps every V2 request failure on the stable JSON error
/// contract instead of leaking Axum's plain-text rejection responses.
pub(crate) struct V2Json<T>(pub(crate) T);

impl<S, T> FromRequest<S> for V2Json<T>
where
    S: Send + Sync,
    Json<T>: FromRequest<S>,
{
    type Rejection = V2ApiError;

    async fn from_request(request: Request, state: &S) -> Result<Self, Self::Rejection> {
        Json::<T>::from_request(request, state)
            .await
            .map(|Json(value)| Self(value))
            .map_err(|_| V2ApiError::bad_request("invalid JSON request"))
    }
}

/// Stable HTTP error translation for the isolated Finance V2 API.
#[derive(Debug)]
pub(crate) struct V2ApiError {
    status: StatusCode,
    message: &'static str,
}

impl V2ApiError {
    pub(crate) fn bad_request(message: &'static str) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message,
        }
    }

    pub(crate) fn unauthorized() -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            message: "unauthorized",
        }
    }

    pub(crate) fn not_found(message: &'static str) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            message,
        }
    }

    pub(crate) fn conflict(message: &'static str) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            message,
        }
    }

    pub(crate) fn internal() -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: "internal server error",
        }
    }
}

impl IntoResponse for V2ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(json!({"error": self.message}))).into_response()
    }
}
