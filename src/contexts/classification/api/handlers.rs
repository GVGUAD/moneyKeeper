use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use chrono::Utc;
use uuid::Uuid;

use crate::api::v2::{AuthenticatedUser, V2ApiError, V2Json};
use crate::contexts::classification::public::{
    CategoryCatalog, CategoryCatalogFacade, CategoryCommand, CategoryId, ClassificationError,
};

use super::dto::{
    CategoryResponse, CreateCategoryRequest, ExpectedVersionRequest, RenameCategoryRequest,
};

pub(crate) async fn create(
    AuthenticatedUser(user_id): AuthenticatedUser,
    State(categories): State<CategoryCatalogFacade>,
    V2Json(request): V2Json<CreateCategoryRequest>,
) -> Result<(StatusCode, Json<CategoryResponse>), V2ApiError> {
    let category = categories
        .create(
            CategoryCommand {
                user_id,
                name: request.name,
                kind: request.kind.into(),
            },
            Utc::now(),
        )
        .await
        .map_err(map_error)?;
    Ok((
        StatusCode::CREATED,
        Json(CategoryResponse::from_view(category, user_id)),
    ))
}

pub(crate) async fn list(
    AuthenticatedUser(user_id): AuthenticatedUser,
    State(categories): State<CategoryCatalogFacade>,
) -> Result<Json<Vec<CategoryResponse>>, V2ApiError> {
    categories
        .list(user_id)
        .await
        .map(|values| {
            Json(
                values
                    .into_iter()
                    .map(|value| CategoryResponse::from_view(value, user_id))
                    .collect(),
            )
        })
        .map_err(map_error)
}

pub(crate) async fn get(
    AuthenticatedUser(user_id): AuthenticatedUser,
    State(categories): State<CategoryCatalogFacade>,
    Path(id): Path<Uuid>,
) -> Result<Json<CategoryResponse>, V2ApiError> {
    categories
        .get(user_id, CategoryId::new(id))
        .await
        .map(|value| Json(CategoryResponse::from_view(value, user_id)))
        .map_err(map_error)
}

pub(crate) async fn rename(
    AuthenticatedUser(user_id): AuthenticatedUser,
    State(categories): State<CategoryCatalogFacade>,
    Path(id): Path<Uuid>,
    V2Json(request): V2Json<RenameCategoryRequest>,
) -> Result<Json<CategoryResponse>, V2ApiError> {
    let expected_version = validate_expected_version(request.expected_version)?;
    categories
        .rename(
            user_id,
            CategoryId::new(id),
            request.name,
            expected_version,
            Utc::now(),
        )
        .await
        .map(|value| Json(CategoryResponse::from_view(value, user_id)))
        .map_err(map_error)
}

pub(crate) async fn archive(
    AuthenticatedUser(user_id): AuthenticatedUser,
    State(categories): State<CategoryCatalogFacade>,
    Path(id): Path<Uuid>,
    V2Json(request): V2Json<ExpectedVersionRequest>,
) -> Result<Json<CategoryResponse>, V2ApiError> {
    let expected_version = validate_expected_version(request.expected_version)?;
    categories
        .archive(user_id, CategoryId::new(id), expected_version, Utc::now())
        .await
        .map(|value| Json(CategoryResponse::from_view(value, user_id)))
        .map_err(map_error)
}

pub(crate) async fn restore(
    AuthenticatedUser(user_id): AuthenticatedUser,
    State(categories): State<CategoryCatalogFacade>,
    Path(id): Path<Uuid>,
    V2Json(request): V2Json<ExpectedVersionRequest>,
) -> Result<Json<CategoryResponse>, V2ApiError> {
    let expected_version = validate_expected_version(request.expected_version)?;
    categories
        .restore(user_id, CategoryId::new(id), expected_version, Utc::now())
        .await
        .map(|value| Json(CategoryResponse::from_view(value, user_id)))
        .map_err(map_error)
}

fn map_error(error: ClassificationError) -> V2ApiError {
    if error.is_not_found() {
        V2ApiError::not_found("category was not found")
    } else if error.is_duplicate_name() || error.is_version_conflict() || error.is_archived() {
        V2ApiError::conflict("category conflict")
    } else if error.is_invalid_name() {
        V2ApiError::bad_request("invalid category")
    } else {
        debug_assert!(error.is_persistence());
        V2ApiError::internal()
    }
}

fn validate_expected_version(expected_version: i64) -> Result<i64, V2ApiError> {
    if expected_version < 1 {
        return Err(V2ApiError::bad_request(
            "expected_version must be at least 1",
        ));
    }
    Ok(expected_version)
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use axum::response::IntoResponse;

    use crate::contexts::classification::public::ClassificationError;

    use super::{map_error, validate_expected_version};

    #[test]
    fn category_versions_match_the_http_contract() {
        assert_eq!(validate_expected_version(1).unwrap(), 1);
        let response = validate_expected_version(0)
            .expect_err("zero is not an aggregate version")
            .into_response();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn classification_persistence_failures_are_server_errors() {
        let response = map_error(ClassificationError::persistence(
            "classification storage is unavailable",
        ))
        .into_response();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }
}
