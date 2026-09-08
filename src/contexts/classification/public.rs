//! Stable contracts published by the Classification context.

use std::fmt;
use std::future::Future;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::shared_kernel::{IdempotencyKey, UserId};

use super::application;

// Host-only worker capabilities. Their persistence remains private to Classification.
pub(crate) use super::automation::{
    ApplicationClaim, BackfillClaim, ClassificationWorkFacade, ClassificationWorker, StaleTarget,
};

pub use super::automation::{
    AutomationDomainError, AutomationError, BackfillJobId, BackfillJobView, BackfillRange,
    BackfillState, CashFlowKind, ClassificationAutomationFacade, ClassificationCategory,
    ClassificationCategoryKind, ClassificationDecisionId, ClassificationEvidence,
    ClassificationEvidenceInput, ClassificationTargetId, ClassificationTargetStatus,
    ClassifierError, ClassifierFailureClass, Confidence, DecisionState, DecisionSuggestion,
    EnqueueOutcome, FeedbackExample, FeedbackSignal, Prediction, PredictionReason, ReviewAction,
    ReviewCursor, ReviewItem, ReviewPage, RolloutEvidence, TargetOrigin, TargetState,
    ThresholdPolicy, TransactionClassifier,
};

/// Public Classification facade with privately assembled persistence.
#[derive(Clone)]
pub struct CategoryCatalogFacade {
    repository: Arc<dyn application::CategoryRepository>,
}

impl CategoryCatalogFacade {
    pub(crate) fn new(repository: Arc<dyn application::CategoryRepository>) -> Self {
        Self { repository }
    }
}

crate::shared_kernel::define_uuid_id!(pub CategoryId);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IconCatalogEntry {
    pub key: &'static str,
    pub label: &'static str,
}

pub const ICON_CATALOG: &[IconCatalogEntry] = &[
    IconCatalogEntry {
        key: "tag",
        label: "Tag",
    },
    IconCatalogEntry {
        key: "wallet-plus",
        label: "Wallet plus",
    },
    IconCatalogEntry {
        key: "briefcase",
        label: "Briefcase",
    },
    IconCatalogEntry {
        key: "laptop",
        label: "Laptop",
    },
    IconCatalogEntry {
        key: "chart-line",
        label: "Line chart",
    },
    IconCatalogEntry {
        key: "circle-ellipsis",
        label: "Other",
    },
    IconCatalogEntry {
        key: "wallet-minus",
        label: "Wallet minus",
    },
    IconCatalogEntry {
        key: "house",
        label: "House",
    },
    IconCatalogEntry {
        key: "building",
        label: "Building",
    },
    IconCatalogEntry {
        key: "bolt",
        label: "Utilities",
    },
    IconCatalogEntry {
        key: "utensils",
        label: "Food",
    },
    IconCatalogEntry {
        key: "shopping-basket",
        label: "Shopping basket",
    },
    IconCatalogEntry {
        key: "coffee",
        label: "Coffee",
    },
    IconCatalogEntry {
        key: "car",
        label: "Car",
    },
    IconCatalogEntry {
        key: "bus",
        label: "Bus",
    },
    IconCatalogEntry {
        key: "fuel",
        label: "Fuel",
    },
    IconCatalogEntry {
        key: "taxi",
        label: "Taxi",
    },
    IconCatalogEntry {
        key: "heart-pulse",
        label: "Health",
    },
    IconCatalogEntry {
        key: "pill",
        label: "Medicine",
    },
    IconCatalogEntry {
        key: "shopping-bag",
        label: "Shopping bag",
    },
    IconCatalogEntry {
        key: "clapperboard",
        label: "Entertainment",
    },
    IconCatalogEntry {
        key: "repeat",
        label: "Subscription",
    },
    IconCatalogEntry {
        key: "plane",
        label: "Travel",
    },
    IconCatalogEntry {
        key: "graduation-cap",
        label: "Education",
    },
    IconCatalogEntry {
        key: "gift",
        label: "Gift",
    },
    IconCatalogEntry {
        key: "receipt",
        label: "Receipt",
    },
];

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CategoryView {
    pub id: CategoryId,
    pub user_id: UserId,
    pub name: String,
    pub kind: CategoryKind,
    /// Local lifecycle. An active node may still be effectively archived by an ancestor.
    pub lifecycle: CategoryLifecycle,
    pub effective_lifecycle: CategoryLifecycle,
    pub parent_id: Option<CategoryId>,
    pub position: i32,
    pub color: Option<String>,
    pub effective_color: String,
    pub icon: Option<String>,
    pub effective_icon: String,
    pub depth: usize,
    pub path: Vec<String>,
    pub assignable: bool,
    /// Node-local compatibility version. Tree mutations use [`TaxonomyView::version`].
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub as_of: DateTime<Utc>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CategoryNodeView {
    pub category: CategoryView,
    pub children: Vec<CategoryNodeView>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaxonomyView {
    pub user_id: UserId,
    pub version: i64,
    pub starter_template_version: Option<i32>,
    pub roots: Vec<CategoryNodeView>,
    pub as_of: DateTime<Utc>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CategoryNodeResult {
    pub taxonomy_version: i64,
    pub starter_template_version: Option<i32>,
    pub node: CategoryNodeView,
}

/// Holds a taxonomy snapshot stable until a coordinated assignment finishes.
/// Persistence and lock ownership remain private to Classification.
pub struct CategoryTaxonomyGuard {
    pub(crate) taxonomy: super::domain::CategoryTaxonomy,
    pub(crate) _lease: Box<dyn Send>,
}

impl CategoryTaxonomyGuard {
    pub fn version(&self) -> i64 {
        self.taxonomy.version()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CreateCategoryNode {
    pub user_id: UserId,
    pub idempotency_key: IdempotencyKey,
    pub expected_version: i64,
    pub name: String,
    pub kind: CategoryKind,
    pub parent_id: Option<CategoryId>,
    pub position: Option<i32>,
    pub color: Option<String>,
    pub icon: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpdateCategoryNode {
    pub user_id: UserId,
    pub idempotency_key: IdempotencyKey,
    pub id: CategoryId,
    pub expected_version: i64,
    pub name: Option<String>,
    pub color: Option<Option<String>>,
    pub icon: Option<Option<String>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MoveCategoryNode {
    pub user_id: UserId,
    pub idempotency_key: IdempotencyKey,
    pub id: CategoryId,
    pub expected_version: i64,
    pub parent_id: Option<CategoryId>,
    pub target_position: i32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReorderCategoryNodes {
    pub user_id: UserId,
    pub idempotency_key: IdempotencyKey,
    pub expected_version: i64,
    pub parent_id: Option<CategoryId>,
    pub ordered_category_ids: Vec<CategoryId>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SetCategoryNodeLifecycle {
    pub user_id: UserId,
    pub idempotency_key: IdempotencyKey,
    pub id: CategoryId,
    pub expected_version: i64,
    pub lifecycle: CategoryLifecycle,
}

pub trait CategoryCatalog: Send + Sync {
    fn assignment_guard(
        &self,
        user_id: UserId,
    ) -> impl Future<Output = Result<CategoryTaxonomyGuard, ClassificationError>> + Send;

    fn taxonomy(
        &self,
        user_id: UserId,
        now: DateTime<Utc>,
    ) -> impl Future<Output = Result<TaxonomyView, ClassificationError>> + Send;
    fn get_node(
        &self,
        user_id: UserId,
        id: CategoryId,
        now: DateTime<Utc>,
    ) -> impl Future<Output = Result<CategoryNodeResult, ClassificationError>> + Send;
    fn create_node(
        &self,
        command: CreateCategoryNode,
        now: DateTime<Utc>,
    ) -> impl Future<Output = Result<CategoryNodeResult, ClassificationError>> + Send;
    fn update_node(
        &self,
        command: UpdateCategoryNode,
        now: DateTime<Utc>,
    ) -> impl Future<Output = Result<CategoryNodeResult, ClassificationError>> + Send;
    fn move_node(
        &self,
        command: MoveCategoryNode,
        now: DateTime<Utc>,
    ) -> impl Future<Output = Result<CategoryNodeResult, ClassificationError>> + Send;
    fn reorder_nodes(
        &self,
        command: ReorderCategoryNodes,
        now: DateTime<Utc>,
    ) -> impl Future<Output = Result<TaxonomyView, ClassificationError>> + Send;
    fn archive_node(
        &self,
        command: SetCategoryNodeLifecycle,
        now: DateTime<Utc>,
    ) -> impl Future<Output = Result<CategoryNodeResult, ClassificationError>> + Send;
    fn restore_node(
        &self,
        command: SetCategoryNodeLifecycle,
        now: DateTime<Utc>,
    ) -> impl Future<Output = Result<CategoryNodeResult, ClassificationError>> + Send;
    fn require_assignable(
        &self,
        user_id: UserId,
        id: CategoryId,
        transaction_kind: CategoryKind,
    ) -> impl Future<Output = Result<CategoryView, ClassificationError>> + Send;
    fn resolve_subtree(
        &self,
        user_id: UserId,
        id: CategoryId,
    ) -> impl Future<Output = Result<Vec<CategoryId>, ClassificationError>> + Send;
}

impl CategoryCatalog for CategoryCatalogFacade {
    async fn assignment_guard(
        &self,
        user_id: UserId,
    ) -> Result<CategoryTaxonomyGuard, ClassificationError> {
        application::assignment_guard(self.repository.as_ref(), user_id).await
    }

    async fn taxonomy(
        &self,
        user_id: UserId,
        now: DateTime<Utc>,
    ) -> Result<TaxonomyView, ClassificationError> {
        application::taxonomy(self.repository.as_ref(), user_id, now).await
    }
    async fn get_node(
        &self,
        user_id: UserId,
        id: CategoryId,
        now: DateTime<Utc>,
    ) -> Result<CategoryNodeResult, ClassificationError> {
        application::get_node(self.repository.as_ref(), user_id, id, now).await
    }
    async fn create_node(
        &self,
        command: CreateCategoryNode,
        now: DateTime<Utc>,
    ) -> Result<CategoryNodeResult, ClassificationError> {
        application::create_node(self.repository.as_ref(), command, now).await
    }
    async fn update_node(
        &self,
        command: UpdateCategoryNode,
        now: DateTime<Utc>,
    ) -> Result<CategoryNodeResult, ClassificationError> {
        application::update_node(self.repository.as_ref(), command, now).await
    }
    async fn move_node(
        &self,
        command: MoveCategoryNode,
        now: DateTime<Utc>,
    ) -> Result<CategoryNodeResult, ClassificationError> {
        application::move_node(self.repository.as_ref(), command, now).await
    }
    async fn reorder_nodes(
        &self,
        command: ReorderCategoryNodes,
        now: DateTime<Utc>,
    ) -> Result<TaxonomyView, ClassificationError> {
        application::reorder_nodes(self.repository.as_ref(), command, now).await
    }
    async fn archive_node(
        &self,
        mut command: SetCategoryNodeLifecycle,
        now: DateTime<Utc>,
    ) -> Result<CategoryNodeResult, ClassificationError> {
        command.lifecycle = CategoryLifecycle::Archived;
        application::set_node_lifecycle(self.repository.as_ref(), command, now).await
    }
    async fn restore_node(
        &self,
        mut command: SetCategoryNodeLifecycle,
        now: DateTime<Utc>,
    ) -> Result<CategoryNodeResult, ClassificationError> {
        command.lifecycle = CategoryLifecycle::Active;
        application::set_node_lifecycle(self.repository.as_ref(), command, now).await
    }
    async fn require_assignable(
        &self,
        user_id: UserId,
        id: CategoryId,
        transaction_kind: CategoryKind,
    ) -> Result<CategoryView, ClassificationError> {
        application::require_assignable(self.repository.as_ref(), user_id, id, transaction_kind)
            .await
    }
    async fn resolve_subtree(
        &self,
        user_id: UserId,
        id: CategoryId,
    ) -> Result<Vec<CategoryId>, ClassificationError> {
        application::resolve_subtree(self.repository.as_ref(), user_id, id).await
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ClassificationErrorKind {
    NotFound,
    InvalidName,
    InvalidStructure,
    InvalidStyle,
    Archived,
    LifecycleConflict,
    NotAssignable,
    DuplicateName,
    VersionConflict,
    IdempotencyConflict,
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
    pub(crate) fn invalid_structure(message: &'static str) -> Self {
        Self::new(ClassificationErrorKind::InvalidStructure, message)
    }
    pub(crate) fn invalid_style() -> Self {
        Self::new(
            ClassificationErrorKind::InvalidStyle,
            "category color or icon is invalid",
        )
    }
    pub(crate) fn archived() -> Self {
        Self::new(
            ClassificationErrorKind::Archived,
            "category is effectively archived",
        )
    }
    pub(crate) fn lifecycle_conflict() -> Self {
        Self::new(
            ClassificationErrorKind::LifecycleConflict,
            "category already has the requested lifecycle",
        )
    }
    pub(crate) fn not_assignable() -> Self {
        Self::new(
            ClassificationErrorKind::NotAssignable,
            "category is not an active compatible leaf",
        )
    }
    pub(crate) fn version_conflict() -> Self {
        Self::new(
            ClassificationErrorKind::VersionConflict,
            "category taxonomy version conflict",
        )
    }
    pub(crate) fn idempotency_conflict() -> Self {
        Self::new(
            ClassificationErrorKind::IdempotencyConflict,
            "idempotency key was reused with different category command content",
        )
    }
    pub(crate) fn persistence(message: &'static str) -> Self {
        Self::new(ClassificationErrorKind::Persistence, message)
    }
    pub(crate) fn duplicate_name(source: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self::new(
            ClassificationErrorKind::DuplicateName,
            "an active sibling category with that name already exists",
        )
        .with_source(source)
    }
    pub(crate) fn duplicate_name_without_source() -> Self {
        Self::new(
            ClassificationErrorKind::DuplicateName,
            "an active sibling category with that name already exists",
        )
    }
    pub(crate) fn storage(source: impl std::error::Error + Send + Sync + 'static) -> Self {
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
    pub fn is_invalid_structure(&self) -> bool {
        self.kind == ClassificationErrorKind::InvalidStructure
    }
    pub fn is_invalid_style(&self) -> bool {
        self.kind == ClassificationErrorKind::InvalidStyle
    }
    pub fn is_duplicate_name(&self) -> bool {
        self.kind == ClassificationErrorKind::DuplicateName
    }
    pub fn is_version_conflict(&self) -> bool {
        self.kind == ClassificationErrorKind::VersionConflict
    }
    pub fn is_idempotency_conflict(&self) -> bool {
        self.kind == ClassificationErrorKind::IdempotencyConflict
    }
    pub fn is_archived(&self) -> bool {
        self.kind == ClassificationErrorKind::Archived
    }
    pub fn is_lifecycle_conflict(&self) -> bool {
        self.kind == ClassificationErrorKind::LifecycleConflict
    }
    pub fn is_not_assignable(&self) -> bool {
        self.kind == ClassificationErrorKind::NotAssignable
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
