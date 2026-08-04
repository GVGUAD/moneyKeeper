use axum::Json;
use axum::extract::{Extension, State};

use crate::api::dto::{UpdateUserSettingsRequest, UserSettingsResponse};
use crate::api::error::AppError;
use crate::api::middleware::AuthUser;
use crate::api::state::AppState;

pub async fn get_settings(
    State(state): State<AppState>,
    Extension(AuthUser(user_id)): Extension<AuthUser>,
) -> Result<Json<UserSettingsResponse>, AppError> {
    let s = state.user_settings.get_or_default(user_id).await?;
    Ok(Json(UserSettingsResponse {
        base_currency: s.base_currency,
    }))
}

pub async fn update_settings(
    State(state): State<AppState>,
    Extension(AuthUser(user_id)): Extension<AuthUser>,
    Json(req): Json<UpdateUserSettingsRequest>,
) -> Result<Json<UserSettingsResponse>, AppError> {
    let s = state
        .user_settings
        .set_base_currency(user_id, &req.base_currency)
        .await?;
    Ok(Json(UserSettingsResponse {
        base_currency: s.base_currency,
    }))
}
