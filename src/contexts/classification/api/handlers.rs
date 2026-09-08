use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use chrono::Utc;
use uuid::Uuid;

use crate::api::{ApiError, ApiJson, AuthenticatedUser};
use crate::contexts::classification::public::{
    CategoryCatalog, CategoryCatalogFacade, CategoryId, CategoryLifecycle, ClassificationError,
    CreateCategoryNode, ICON_CATALOG, MoveCategoryNode, ReorderCategoryNodes,
    SetCategoryNodeLifecycle, UpdateCategoryNode,
};
use crate::shared_kernel::IdempotencyKey;

use super::dto::{
    CategoryNodeResultResponse, CreateCategoryRequest, ExpectedVersionRequest, IconResponse,
    MoveCategoryRequest, ReorderCategoriesRequest, TaxonomyResponse, UpdateCategoryRequest,
};

pub(crate) async fn create(
    AuthenticatedUser(user_id): AuthenticatedUser,
    State(categories): State<CategoryCatalogFacade>,
    headers: HeaderMap,
    ApiJson(request): ApiJson<CreateCategoryRequest>,
) -> Result<(StatusCode, Json<CategoryNodeResultResponse>), ApiError> {
    let result = categories
        .create_node(
            CreateCategoryNode {
                user_id,
                idempotency_key: idempotency_key(&headers)?,
                expected_version: expected_version(request.expected_version)?,
                name: request.name,
                kind: request.kind,
                parent_id: request.parent_id.map(CategoryId::new),
                position: request.position,
                color: request.color,
                icon: request.icon,
            },
            Utc::now(),
        )
        .await
        .map_err(map_error)?;
    Ok((
        StatusCode::CREATED,
        Json(CategoryNodeResultResponse::from_view(result, user_id)),
    ))
}

pub(crate) async fn list(
    AuthenticatedUser(user_id): AuthenticatedUser,
    State(categories): State<CategoryCatalogFacade>,
) -> Result<Json<TaxonomyResponse>, ApiError> {
    categories
        .taxonomy(user_id, Utc::now())
        .await
        .map(|value| Json(TaxonomyResponse::from_view(value, user_id)))
        .map_err(map_error)
}

pub(crate) async fn get(
    AuthenticatedUser(user_id): AuthenticatedUser,
    State(categories): State<CategoryCatalogFacade>,
    Path(id): Path<Uuid>,
) -> Result<Json<CategoryNodeResultResponse>, ApiError> {
    categories
        .get_node(user_id, CategoryId::new(id), Utc::now())
        .await
        .map(|value| Json(CategoryNodeResultResponse::from_view(value, user_id)))
        .map_err(map_error)
}

pub(crate) async fn update(
    AuthenticatedUser(user_id): AuthenticatedUser,
    State(categories): State<CategoryCatalogFacade>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    ApiJson(request): ApiJson<UpdateCategoryRequest>,
) -> Result<Json<CategoryNodeResultResponse>, ApiError> {
    categories
        .update_node(
            UpdateCategoryNode {
                user_id,
                idempotency_key: idempotency_key(&headers)?,
                id: CategoryId::new(id),
                expected_version: expected_version(request.expected_version)?,
                name: request.name,
                color: request.color.into_nested_option(),
                icon: request.icon.into_nested_option(),
            },
            Utc::now(),
        )
        .await
        .map(|value| Json(CategoryNodeResultResponse::from_view(value, user_id)))
        .map_err(map_error)
}

pub(crate) async fn move_node(
    AuthenticatedUser(user_id): AuthenticatedUser,
    State(categories): State<CategoryCatalogFacade>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    ApiJson(request): ApiJson<MoveCategoryRequest>,
) -> Result<Json<CategoryNodeResultResponse>, ApiError> {
    categories
        .move_node(
            MoveCategoryNode {
                user_id,
                idempotency_key: idempotency_key(&headers)?,
                id: CategoryId::new(id),
                expected_version: expected_version(request.expected_version)?,
                parent_id: request.parent_id.map(CategoryId::new),
                target_position: request.target_position,
            },
            Utc::now(),
        )
        .await
        .map(|value| Json(CategoryNodeResultResponse::from_view(value, user_id)))
        .map_err(map_error)
}

pub(crate) async fn reorder(
    AuthenticatedUser(user_id): AuthenticatedUser,
    State(categories): State<CategoryCatalogFacade>,
    headers: HeaderMap,
    ApiJson(request): ApiJson<ReorderCategoriesRequest>,
) -> Result<Json<TaxonomyResponse>, ApiError> {
    categories
        .reorder_nodes(
            ReorderCategoryNodes {
                user_id,
                idempotency_key: idempotency_key(&headers)?,
                expected_version: expected_version(request.expected_version)?,
                parent_id: request.parent_id.map(CategoryId::new),
                ordered_category_ids: request
                    .ordered_category_ids
                    .into_iter()
                    .map(CategoryId::new)
                    .collect(),
            },
            Utc::now(),
        )
        .await
        .map(|value| Json(TaxonomyResponse::from_view(value, user_id)))
        .map_err(map_error)
}

pub(crate) async fn archive(
    user: AuthenticatedUser,
    state: State<CategoryCatalogFacade>,
    path: Path<Uuid>,
    headers: HeaderMap,
    request: ApiJson<ExpectedVersionRequest>,
) -> Result<Json<CategoryNodeResultResponse>, ApiError> {
    set_lifecycle(
        user,
        state,
        path,
        headers,
        request,
        CategoryLifecycle::Archived,
    )
    .await
}

pub(crate) async fn restore(
    user: AuthenticatedUser,
    state: State<CategoryCatalogFacade>,
    path: Path<Uuid>,
    headers: HeaderMap,
    request: ApiJson<ExpectedVersionRequest>,
) -> Result<Json<CategoryNodeResultResponse>, ApiError> {
    set_lifecycle(
        user,
        state,
        path,
        headers,
        request,
        CategoryLifecycle::Active,
    )
    .await
}

async fn set_lifecycle(
    AuthenticatedUser(user_id): AuthenticatedUser,
    State(categories): State<CategoryCatalogFacade>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    ApiJson(request): ApiJson<ExpectedVersionRequest>,
    lifecycle: CategoryLifecycle,
) -> Result<Json<CategoryNodeResultResponse>, ApiError> {
    let command = SetCategoryNodeLifecycle {
        user_id,
        idempotency_key: idempotency_key(&headers)?,
        id: CategoryId::new(id),
        expected_version: expected_version(request.expected_version)?,
        lifecycle,
    };
    let result = match lifecycle {
        CategoryLifecycle::Active => categories.restore_node(command, Utc::now()).await,
        CategoryLifecycle::Archived => categories.archive_node(command, Utc::now()).await,
    }
    .map_err(map_error)?;
    Ok(Json(CategoryNodeResultResponse::from_view(result, user_id)))
}

pub(crate) async fn icons() -> Json<Vec<IconResponse>> {
    Json(
        ICON_CATALOG
            .iter()
            .copied()
            .map(IconResponse::from)
            .collect(),
    )
}

fn idempotency_key(headers: &HeaderMap) -> Result<IdempotencyKey, ApiError> {
    let value = headers
        .get("Idempotency-Key")
        .ok_or_else(|| ApiError::bad_request("missing Idempotency-Key header"))?
        .to_str()
        .map_err(|_| ApiError::bad_request("invalid Idempotency-Key header"))?;
    IdempotencyKey::new(value).map_err(|_| ApiError::bad_request("invalid Idempotency-Key header"))
}

pub(super) fn map_error(error: ClassificationError) -> ApiError {
    if error.is_not_found() {
        ApiError::not_found("category was not found")
    } else if error.is_duplicate_name()
        || error.is_version_conflict()
        || error.is_idempotency_conflict()
        || error.is_archived()
        || error.is_lifecycle_conflict()
        || error.is_not_assignable()
    {
        ApiError::conflict("category conflict")
    } else if error.is_invalid_name() || error.is_invalid_structure() || error.is_invalid_style() {
        ApiError::bad_request("invalid category")
    } else {
        debug_assert!(error.is_persistence());
        ApiError::internal(
            "classification.persistence",
            "classification storage operation failed",
        )
    }
}

fn expected_version(value: i64) -> Result<i64, ApiError> {
    if value < 1 {
        return Err(ApiError::bad_request("expected_version must be at least 1"));
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use axum::response::IntoResponse as _;

    use crate::contexts::classification::public::ClassificationError;

    use super::{expected_version, map_error};

    #[test]
    fn taxonomy_versions_match_the_http_contract() {
        assert_eq!(expected_version(1).unwrap(), 1);
        assert_eq!(
            expected_version(0).unwrap_err().into_response().status(),
            StatusCode::BAD_REQUEST
        );
    }

    #[test]
    fn structural_errors_are_client_errors() {
        assert_eq!(
            map_error(ClassificationError::invalid_structure("cycle"))
                .into_response()
                .status(),
            StatusCode::BAD_REQUEST
        );
    }
}
