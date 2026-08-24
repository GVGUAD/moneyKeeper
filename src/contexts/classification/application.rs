use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::shared_kernel::UserId;

use super::domain::Category;
use super::public::{CategoryCommand, CategoryId, CategoryView, ClassificationError};

#[async_trait]
pub(crate) trait CategoryRepository: Send + Sync {
    async fn insert(&self, category: &Category) -> Result<(), ClassificationError>;
    async fn find(
        &self,
        user_id: UserId,
        id: CategoryId,
    ) -> Result<Option<Category>, ClassificationError>;
    async fn list_for_user(&self, user_id: UserId) -> Result<Vec<Category>, ClassificationError>;
    async fn update(
        &self,
        category: &Category,
        expected_version: i64,
    ) -> Result<(), ClassificationError>;
}

pub(crate) async fn create<R: CategoryRepository + ?Sized>(
    categories: &R,
    command: CategoryCommand,
    now: DateTime<Utc>,
) -> Result<CategoryView, ClassificationError> {
    let category = Category::create(
        CategoryId::generate(),
        command.user_id,
        command.name,
        command.kind,
        now,
    )?;
    categories.insert(&category).await?;
    Ok(category.into())
}

pub(crate) async fn get<R: CategoryRepository + ?Sized>(
    categories: &R,
    user_id: UserId,
    id: CategoryId,
) -> Result<CategoryView, ClassificationError> {
    categories
        .find(user_id, id)
        .await?
        .map(Into::into)
        .ok_or_else(ClassificationError::not_found)
}

pub(crate) async fn list<R: CategoryRepository + ?Sized>(
    categories: &R,
    user_id: UserId,
) -> Result<Vec<CategoryView>, ClassificationError> {
    categories
        .list_for_user(user_id)
        .await
        .map(|values| values.into_iter().map(Into::into).collect())
}

pub(crate) async fn rename<R: CategoryRepository + ?Sized>(
    categories: &R,
    user_id: UserId,
    id: CategoryId,
    name: String,
    expected_version: i64,
    now: DateTime<Utc>,
) -> Result<CategoryView, ClassificationError> {
    let mut category = categories
        .find(user_id, id)
        .await?
        .ok_or_else(ClassificationError::not_found)?;
    category.rename(name, expected_version, now)?;
    // Even an idempotent rename must cross the database compare-and-swap
    // boundary. Otherwise a concurrent writer can advance the stored version
    // after `find` while this command still returns the stale snapshot as a
    // successful no-op.
    categories.update(&category, expected_version).await?;
    Ok(category.into())
}

pub(crate) async fn archive<R: CategoryRepository + ?Sized>(
    categories: &R,
    user_id: UserId,
    id: CategoryId,
    expected_version: i64,
    now: DateTime<Utc>,
) -> Result<CategoryView, ClassificationError> {
    let mut category = categories
        .find(user_id, id)
        .await?
        .ok_or_else(ClassificationError::not_found)?;
    category.archive(expected_version, now)?;
    categories.update(&category, expected_version).await?;
    Ok(category.into())
}

pub(crate) async fn restore<R: CategoryRepository + ?Sized>(
    categories: &R,
    user_id: UserId,
    id: CategoryId,
    expected_version: i64,
    now: DateTime<Utc>,
) -> Result<CategoryView, ClassificationError> {
    let mut category = categories
        .find(user_id, id)
        .await?
        .ok_or_else(ClassificationError::not_found)?;
    category.restore(expected_version, now)?;
    categories.update(&category, expected_version).await?;
    Ok(category.into())
}

pub(crate) async fn require_active<R: CategoryRepository + ?Sized>(
    categories: &R,
    user_id: UserId,
    id: CategoryId,
) -> Result<CategoryView, ClassificationError> {
    let category = categories
        .find(user_id, id)
        .await?
        .ok_or_else(ClassificationError::not_found)?;
    if category.lifecycle() != super::public::CategoryLifecycle::Active {
        return Err(ClassificationError::archived());
    }
    Ok(category.into())
}

impl From<Category> for CategoryView {
    fn from(category: Category) -> Self {
        Self {
            id: category.id(),
            user_id: category.user_id(),
            name: category.name().to_owned(),
            kind: category.kind(),
            lifecycle: category.lifecycle(),
            version: category.version(),
            created_at: category.created_at(),
            as_of: category.updated_at(),
        }
    }
}
