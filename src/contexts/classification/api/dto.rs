use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::contexts::classification::public::{CategoryKind, CategoryLifecycle, CategoryView};
use crate::shared_kernel::UserId;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CreateCategoryRequest {
    pub(crate) name: String,
    pub(crate) kind: CategoryKindDto,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CategoryKindDto {
    Income,
    Expense,
    Both,
}

impl From<CategoryKindDto> for CategoryKind {
    fn from(value: CategoryKindDto) -> Self {
        match value {
            CategoryKindDto::Income => Self::Income,
            CategoryKindDto::Expense => Self::Expense,
            CategoryKindDto::Both => Self::Both,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RenameCategoryRequest {
    pub(crate) name: String,
    pub(crate) expected_version: i64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExpectedVersionRequest {
    pub(crate) expected_version: i64,
}

#[derive(Debug, Serialize)]
pub(crate) struct CategoryResponse {
    id: String,
    name: String,
    kind: &'static str,
    lifecycle: &'static str,
    version: i64,
    created_at: DateTime<Utc>,
    as_of: DateTime<Utc>,
}

impl CategoryResponse {
    pub(crate) fn from_view(value: CategoryView, authenticated_user: UserId) -> Self {
        debug_assert_eq!(value.user_id, authenticated_user);
        let kind = match value.kind {
            CategoryKind::Income => "income",
            CategoryKind::Expense => "expense",
            CategoryKind::Both => "both",
        };
        let lifecycle = match value.lifecycle {
            CategoryLifecycle::Active => "active",
            CategoryLifecycle::Archived => "archived",
        };
        Self {
            id: value.id.to_string(),
            name: value.name,
            kind,
            lifecycle,
            version: value.version,
            created_at: value.created_at,
            as_of: value.as_of,
        }
    }
}
