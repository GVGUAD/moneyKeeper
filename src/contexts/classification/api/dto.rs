use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize};
use uuid::Uuid;

use crate::contexts::classification::public::{
    CategoryKind, CategoryLifecycle, CategoryNodeResult, CategoryNodeView, IconCatalogEntry,
    TaxonomyView,
};
use crate::shared_kernel::UserId;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CreateCategoryRequest {
    pub(crate) expected_version: i64,
    pub(crate) name: String,
    pub(crate) kind: CategoryKind,
    #[serde(default)]
    pub(crate) parent_id: Option<Uuid>,
    #[serde(default)]
    pub(crate) position: Option<i32>,
    #[serde(default)]
    pub(crate) color: Option<String>,
    #[serde(default)]
    pub(crate) icon: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum PatchField<T> {
    #[default]
    Unset,
    Clear,
    Set(T),
}

impl<'de, T> Deserialize<'de> for PatchField<T>
where
    T: Deserialize<'de>,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Option::<T>::deserialize(deserializer).map(|value| match value {
            Some(value) => Self::Set(value),
            None => Self::Clear,
        })
    }
}

impl<T> PatchField<T> {
    pub(crate) fn into_nested_option(self) -> Option<Option<T>> {
        match self {
            Self::Unset => None,
            Self::Clear => Some(None),
            Self::Set(value) => Some(Some(value)),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct UpdateCategoryRequest {
    pub(crate) expected_version: i64,
    #[serde(default)]
    pub(crate) name: Option<String>,
    #[serde(default)]
    pub(crate) color: PatchField<String>,
    #[serde(default)]
    pub(crate) icon: PatchField<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MoveCategoryRequest {
    pub(crate) expected_version: i64,
    #[serde(default)]
    pub(crate) parent_id: Option<Uuid>,
    pub(crate) target_position: i32,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReorderCategoriesRequest {
    pub(crate) expected_version: i64,
    #[serde(default)]
    pub(crate) parent_id: Option<Uuid>,
    pub(crate) ordered_category_ids: Vec<Uuid>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExpectedVersionRequest {
    pub(crate) expected_version: i64,
}

#[derive(Debug, Serialize)]
pub(crate) struct TaxonomyResponse {
    version: i64,
    starter_template_version: Option<i32>,
    roots: Vec<CategoryNodeResponse>,
}

impl TaxonomyResponse {
    pub(crate) fn from_view(value: TaxonomyView, user_id: UserId) -> Self {
        debug_assert_eq!(value.user_id, user_id);
        Self {
            version: value.version,
            starter_template_version: value.starter_template_version,
            roots: value
                .roots
                .into_iter()
                .map(CategoryNodeResponse::from_view)
                .collect(),
        }
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct CategoryNodeResultResponse {
    version: i64,
    starter_template_version: Option<i32>,
    node: CategoryNodeResponse,
}

impl CategoryNodeResultResponse {
    pub(crate) fn from_view(value: CategoryNodeResult, user_id: UserId) -> Self {
        debug_assert_eq!(value.node.category.user_id, user_id);
        Self {
            version: value.taxonomy_version,
            starter_template_version: value.starter_template_version,
            node: CategoryNodeResponse::from_view(value.node),
        }
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct CategoryNodeResponse {
    id: Uuid,
    parent_id: Option<Uuid>,
    name: String,
    kind: CategoryKind,
    local_lifecycle: CategoryLifecycle,
    effective_lifecycle: CategoryLifecycle,
    position: i32,
    color: Option<String>,
    effective_color: String,
    icon: Option<String>,
    effective_icon: String,
    path: Vec<String>,
    depth: usize,
    assignable: bool,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    children: Vec<CategoryNodeResponse>,
}

impl CategoryNodeResponse {
    fn from_view(value: CategoryNodeView) -> Self {
        let category = value.category;
        Self {
            id: category.id.into_uuid(),
            parent_id: category.parent_id.map(|id| id.into_uuid()),
            name: category.name,
            kind: category.kind,
            local_lifecycle: category.lifecycle,
            effective_lifecycle: category.effective_lifecycle,
            position: category.position,
            color: category.color,
            effective_color: category.effective_color,
            icon: category.icon,
            effective_icon: category.effective_icon,
            path: category.path,
            depth: category.depth,
            assignable: category.assignable,
            created_at: category.created_at,
            updated_at: category.as_of,
            children: value.children.into_iter().map(Self::from_view).collect(),
        }
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct IconResponse {
    key: &'static str,
    label: &'static str,
}

impl From<IconCatalogEntry> for IconResponse {
    fn from(value: IconCatalogEntry) -> Self {
        Self {
            key: value.key,
            label: value.label,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{PatchField, UpdateCategoryRequest};

    #[test]
    fn patch_distinguishes_omitted_style_from_explicit_null() {
        let omitted: UpdateCategoryRequest =
            serde_json::from_str(r#"{"expected_version":1}"#).unwrap();
        assert_eq!(omitted.color, PatchField::Unset);
        let cleared: UpdateCategoryRequest =
            serde_json::from_str(r#"{"expected_version":1,"color":null}"#).unwrap();
        assert_eq!(cleared.color, PatchField::Clear);
        let set: UpdateCategoryRequest =
            serde_json::from_str(r##"{"expected_version":1,"color":"#abcdef"}"##).unwrap();
        assert_eq!(set.color, PatchField::Set("#abcdef".to_owned()));
    }
}
