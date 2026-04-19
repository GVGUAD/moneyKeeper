use axum::Json;
use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use uuid::Uuid;

use crate::api::dto::{CategoryResponse, CreateCategoryRequest, UpdateCategoryRequest};
use crate::api::error::AppError;
use crate::api::middleware::AuthUser;
use crate::api::state::AppState;

pub async fn create_category(
    State(state): State<AppState>,
    Extension(AuthUser(user_id)): Extension<AuthUser>,
    Json(req): Json<CreateCategoryRequest>,
) -> Result<(StatusCode, Json<CategoryResponse>), AppError> {
    let cat = state
        .categories
        .create(user_id, req.name, req.color)
        .await?;
    Ok((
        StatusCode::CREATED,
        Json(CategoryResponse {
            id: cat.id,
            name: cat.name,
            color: cat.color,
            created_at: cat.created_at,
        }),
    ))
}

pub async fn list_categories(
    State(state): State<AppState>,
    Extension(AuthUser(user_id)): Extension<AuthUser>,
) -> Result<Json<Vec<CategoryResponse>>, AppError> {
    let cats = state.categories.list(user_id).await?;
    Ok(Json(
        cats.into_iter()
            .map(|c| CategoryResponse {
                id: c.id,
                name: c.name,
                color: c.color,
                created_at: c.created_at,
            })
            .collect(),
    ))
}

pub async fn update_category(
    State(state): State<AppState>,
    Extension(AuthUser(user_id)): Extension<AuthUser>,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdateCategoryRequest>,
) -> Result<Json<CategoryResponse>, AppError> {
    let cat = state
        .categories
        .update(id, user_id, req.name, req.color)
        .await?;
    Ok(Json(CategoryResponse {
        id: cat.id,
        name: cat.name,
        color: cat.color,
        created_at: cat.created_at,
    }))
}

pub async fn delete_category(
    State(state): State<AppState>,
    Extension(AuthUser(user_id)): Extension<AuthUser>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, AppError> {
    state.categories.delete(id, user_id).await?;
    Ok(StatusCode::NO_CONTENT)
}
