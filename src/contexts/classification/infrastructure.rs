use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use crate::shared_kernel::UserId;

use super::domain::Category;
use super::public::{CategoryId, CategoryKind, CategoryLifecycle, ClassificationError};

#[derive(sqlx::FromRow)]
struct CategoryRow {
    id: Uuid,
    user_id: Uuid,
    name: String,
    kind: String,
    lifecycle: String,
    version: i64,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl CategoryRow {
    fn into_domain(self) -> Result<Category, ClassificationError> {
        Category::reconstitute(
            CategoryId::new(self.id),
            UserId::new(self.user_id),
            self.name,
            CategoryKind::parse(&self.kind)?,
            CategoryLifecycle::parse(&self.lifecycle)?,
            self.version,
            self.created_at,
            self.updated_at,
        )
    }
}

/// PostgreSQL-backed category capability.
#[derive(Clone)]
pub(crate) struct PgCategoryCatalog {
    pool: PgPool,
}

impl PgCategoryCatalog {
    /// Creates a category capability backed by a Finance V2 pool.
    pub(crate) fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub(crate) async fn insert(&self, category: &Category) -> Result<(), ClassificationError> {
        sqlx::query(
            "INSERT INTO classification.categories \
             (id, user_id, name, kind, lifecycle, version, created_at, updated_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(category.id().into_uuid())
        .bind(category.user_id().into_uuid())
        .bind(category.name())
        .bind(category.kind().as_str())
        .bind(category.lifecycle().as_str())
        .bind(category.version())
        .bind(category.created_at())
        .bind(category.updated_at())
        .execute(&self.pool)
        .await
        .map_err(ClassificationError::database)?;
        Ok(())
    }

    pub(crate) async fn find(
        &self,
        user_id: UserId,
        id: CategoryId,
    ) -> Result<Option<Category>, ClassificationError> {
        sqlx::query_as::<_, CategoryRow>(
            "SELECT id, user_id, name, kind, lifecycle, version, created_at, updated_at \
             FROM classification.categories WHERE id = $1 AND user_id = $2",
        )
        .bind(id.into_uuid())
        .bind(user_id.into_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(ClassificationError::database)?
        .map(CategoryRow::into_domain)
        .transpose()
    }

    pub(crate) async fn list_for_user(
        &self,
        user_id: UserId,
    ) -> Result<Vec<Category>, ClassificationError> {
        sqlx::query_as::<_, CategoryRow>(
            "SELECT id, user_id, name, kind, lifecycle, version, created_at, updated_at \
             FROM classification.categories WHERE user_id = $1 \
             ORDER BY lower(name), id",
        )
        .bind(user_id.into_uuid())
        .fetch_all(&self.pool)
        .await
        .map_err(ClassificationError::database)?
        .into_iter()
        .map(CategoryRow::into_domain)
        .collect()
    }

    pub(crate) async fn update(
        &self,
        category: &Category,
        expected_version: i64,
    ) -> Result<(), ClassificationError> {
        let result = sqlx::query(
            "UPDATE classification.categories \
             SET name = $1, kind = $2, lifecycle = $3, version = $4, updated_at = $5 \
             WHERE id = $6 AND user_id = $7 AND version = $8",
        )
        .bind(category.name())
        .bind(category.kind().as_str())
        .bind(category.lifecycle().as_str())
        .bind(category.version())
        .bind(category.updated_at())
        .bind(category.id().into_uuid())
        .bind(category.user_id().into_uuid())
        .bind(expected_version)
        .execute(&self.pool)
        .await
        .map_err(ClassificationError::database)?;
        if result.rows_affected() == 0 {
            return Err(ClassificationError::version_conflict());
        }
        Ok(())
    }
}
