//! Stable contracts published by the Classification context.

use std::fmt;
use std::future::Future;

use chrono::{DateTime, Utc};

use crate::shared_kernel::UserId;

use super::application;
use super::infrastructure::PgCategoryCatalog;

/// Public Classification facade with privately assembled persistence.
#[derive(Clone)]
pub struct CategoryCatalogFacade {
    adapter: PgCategoryCatalog,
}

impl CategoryCatalogFacade {
    pub(crate) fn new(adapter: PgCategoryCatalog) -> Self {
        Self { adapter }
    }
}

crate::shared_kernel::define_uuid_id!(pub CategoryId);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CategoryKind {
    Income,
    Expense,
    Both,
}

impl CategoryKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Income => "income",
            Self::Expense => "expense",
            Self::Both => "both",
        }
    }

    pub(crate) fn parse(value: &str) -> Result<Self, ClassificationError> {
        match value {
            "income" => Ok(Self::Income),
            "expense" => Ok(Self::Expense),
            "both" => Ok(Self::Both),
            _ => Err(ClassificationError::persistence(
                "stored category kind is invalid",
            )),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CategoryLifecycle {
    Active,
    Archived,
}

impl CategoryLifecycle {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Archived => "archived",
        }
    }

    pub(crate) fn parse(value: &str) -> Result<Self, ClassificationError> {
        match value {
            "active" => Ok(Self::Active),
            "archived" => Ok(Self::Archived),
            _ => Err(ClassificationError::persistence(
                "stored category lifecycle is invalid",
            )),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CategoryView {
    pub id: CategoryId,
    pub user_id: UserId,
    pub name: String,
    pub kind: CategoryKind,
    pub lifecycle: CategoryLifecycle,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub as_of: DateTime<Utc>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CategoryCommand {
    pub user_id: UserId,
    pub name: String,
    pub kind: CategoryKind,
}

pub trait CategoryCatalog: Send + Sync {
    fn create(
        &self,
        command: CategoryCommand,
        now: DateTime<Utc>,
    ) -> impl Future<Output = Result<CategoryView, ClassificationError>> + Send;

    fn get(
        &self,
        user_id: UserId,
        id: CategoryId,
    ) -> impl Future<Output = Result<CategoryView, ClassificationError>> + Send;

    fn list(
        &self,
        user_id: UserId,
    ) -> impl Future<Output = Result<Vec<CategoryView>, ClassificationError>> + Send;

    fn rename(
        &self,
        user_id: UserId,
        id: CategoryId,
        name: String,
        expected_version: i64,
        now: DateTime<Utc>,
    ) -> impl Future<Output = Result<CategoryView, ClassificationError>> + Send;

    fn archive(
        &self,
        user_id: UserId,
        id: CategoryId,
        expected_version: i64,
        now: DateTime<Utc>,
    ) -> impl Future<Output = Result<CategoryView, ClassificationError>> + Send;

    fn restore(
        &self,
        user_id: UserId,
        id: CategoryId,
        expected_version: i64,
        now: DateTime<Utc>,
    ) -> impl Future<Output = Result<CategoryView, ClassificationError>> + Send;

    fn require_active(
        &self,
        user_id: UserId,
        id: CategoryId,
    ) -> impl Future<Output = Result<CategoryView, ClassificationError>> + Send;
}

impl CategoryCatalog for CategoryCatalogFacade {
    async fn create(
        &self,
        command: CategoryCommand,
        now: DateTime<Utc>,
    ) -> Result<CategoryView, ClassificationError> {
        application::create(&self.adapter, command, now).await
    }

    async fn get(
        &self,
        user_id: UserId,
        id: CategoryId,
    ) -> Result<CategoryView, ClassificationError> {
        application::get(&self.adapter, user_id, id).await
    }

    async fn list(&self, user_id: UserId) -> Result<Vec<CategoryView>, ClassificationError> {
        application::list(&self.adapter, user_id).await
    }

    async fn rename(
        &self,
        user_id: UserId,
        id: CategoryId,
        name: String,
        expected_version: i64,
        now: DateTime<Utc>,
    ) -> Result<CategoryView, ClassificationError> {
        application::rename(&self.adapter, user_id, id, name, expected_version, now).await
    }

    async fn archive(
        &self,
        user_id: UserId,
        id: CategoryId,
        expected_version: i64,
        now: DateTime<Utc>,
    ) -> Result<CategoryView, ClassificationError> {
        application::archive(&self.adapter, user_id, id, expected_version, now).await
    }

    async fn restore(
        &self,
        user_id: UserId,
        id: CategoryId,
        expected_version: i64,
        now: DateTime<Utc>,
    ) -> Result<CategoryView, ClassificationError> {
        application::restore(&self.adapter, user_id, id, expected_version, now).await
    }

    async fn require_active(
        &self,
        user_id: UserId,
        id: CategoryId,
    ) -> Result<CategoryView, ClassificationError> {
        application::require_active(&self.adapter, user_id, id).await
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ClassificationErrorKind {
    NotFound,
    InvalidName,
    Archived,
    DuplicateName,
    VersionConflict,
    Persistence,
}

#[derive(Debug)]
pub struct ClassificationError {
    kind: ClassificationErrorKind,
    message: &'static str,
    cause: Option<Box<dyn std::error::Error + Send + Sync>>,
}

impl ClassificationError {
    pub(crate) fn not_found() -> Self {
        Self::new(ClassificationErrorKind::NotFound, "category was not found")
    }

    pub(crate) fn invalid_name() -> Self {
        Self::new(
            ClassificationErrorKind::InvalidName,
            "category name must contain 1 to 100 characters",
        )
    }

    pub(crate) fn archived() -> Self {
        Self::new(ClassificationErrorKind::Archived, "category is archived")
    }

    pub(crate) fn version_conflict() -> Self {
        Self::new(
            ClassificationErrorKind::VersionConflict,
            "category version conflict",
        )
    }

    pub(crate) fn persistence(message: &'static str) -> Self {
        Self::new(ClassificationErrorKind::Persistence, message)
    }

    pub(crate) fn database(source: sqlx::Error) -> Self {
        let duplicate = source.as_database_error().is_some_and(|error| {
            error.code().as_deref() == Some("23505")
                && error.constraint() == Some("categories_active_name_unique")
        });
        if duplicate {
            return Self::new(
                ClassificationErrorKind::DuplicateName,
                "an active category with that name already exists",
            )
            .with_source(source);
        }
        Self::persistence("classification storage is unavailable").with_source(source)
    }

    fn new(kind: ClassificationErrorKind, message: &'static str) -> Self {
        Self {
            kind,
            message,
            cause: None,
        }
    }

    fn with_source(mut self, source: impl std::error::Error + Send + Sync + 'static) -> Self {
        self.cause = Some(Box::new(source));
        self
    }

    pub fn is_not_found(&self) -> bool {
        self.kind == ClassificationErrorKind::NotFound
    }

    pub fn is_invalid_name(&self) -> bool {
        self.kind == ClassificationErrorKind::InvalidName
    }

    pub fn is_duplicate_name(&self) -> bool {
        self.kind == ClassificationErrorKind::DuplicateName
    }

    pub fn is_version_conflict(&self) -> bool {
        self.kind == ClassificationErrorKind::VersionConflict
    }

    pub fn is_archived(&self) -> bool {
        self.kind == ClassificationErrorKind::Archived
    }

    pub fn is_persistence(&self) -> bool {
        self.kind == ClassificationErrorKind::Persistence
    }
}

impl fmt::Display for ClassificationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for ClassificationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.cause
            .as_deref()
            .map(|source| source as &(dyn std::error::Error + 'static))
    }
}
