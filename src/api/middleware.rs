use axum::extract::{Request, State};
use axum::http::header::AUTHORIZATION;
use axum::middleware::Next;
use axum::response::Response;
use uuid::Uuid;

use crate::api::error::AppError;
use crate::api::jwt::verify_token;
use crate::api::state::AppState;
use crate::domain::error::DomainError;

#[derive(Clone, Debug)]
pub struct AuthUser(pub Uuid);

pub async fn auth_middleware(
    State(state): State<AppState>,
    mut req: Request,
    next: Next,
) -> Result<Response, AppError> {
    let header = req
        .headers()
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .ok_or(DomainError::Unauthorized)?;

    let claims =
        verify_token(header, &state.supabase_jwks).map_err(|_| DomainError::Unauthorized)?;

    let user_id = Uuid::parse_str(&claims.sub).map_err(|_| DomainError::Unauthorized)?;

    req.extensions_mut().insert(AuthUser(user_id));
    Ok(next.run(req).await)
}
