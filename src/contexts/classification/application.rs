use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::shared_kernel::{IdempotencyKey, UserId};

use super::domain::{Category, CategoryTaxonomy};
use super::public::{
    CategoryId, CategoryKind, CategoryLifecycle, CategoryNodeResult, CategoryNodeView,
    CategoryView, ClassificationError, CreateCategoryNode, MoveCategoryNode, ReorderCategoryNodes,
    SetCategoryNodeLifecycle, TaxonomyView, UpdateCategoryNode,
};

#[async_trait]
pub(crate) trait CategoryRepository: Send + Sync {
    async fn lock_taxonomy(
        &self,
        user_id: UserId,
    ) -> Result<super::public::CategoryTaxonomyGuard, ClassificationError>;

    async fn list_for_user(&self, user_id: UserId) -> Result<Vec<Category>, ClassificationError>;

    // Aggregate persistence. Initialization returns false when another caller won the race.
    async fn load_taxonomy(
        &self,
        user_id: UserId,
    ) -> Result<Option<CategoryTaxonomy>, ClassificationError>;
    async fn initialize_taxonomy(
        &self,
        taxonomy: &CategoryTaxonomy,
    ) -> Result<bool, ClassificationError>;
    async fn find_receipt(
        &self,
        user_id: UserId,
        scope: &'static str,
        key: &IdempotencyKey,
        request_hash: &[u8; 32],
    ) -> Result<Option<Value>, ClassificationError>;
    /// Saves the aggregate and receipt atomically. A returned value is a concurrent replay.
    async fn save_taxonomy_idempotent(
        &self,
        taxonomy: &CategoryTaxonomy,
        expected_version: i64,
        scope: &'static str,
        key: &IdempotencyKey,
        request_hash: &[u8; 32],
        response: &Value,
    ) -> Result<Option<Value>, ClassificationError>;
}

pub(crate) async fn assignment_guard<R: CategoryRepository + ?Sized>(
    categories: &R,
    user_id: UserId,
) -> Result<super::public::CategoryTaxonomyGuard, ClassificationError> {
    load_or_initialize(categories, user_id, Utc::now()).await?;
    categories.lock_taxonomy(user_id).await
}

pub(crate) async fn taxonomy<R: CategoryRepository + ?Sized>(
    categories: &R,
    user_id: UserId,
    now: DateTime<Utc>,
) -> Result<TaxonomyView, ClassificationError> {
    let taxonomy = load_or_initialize(categories, user_id, now).await?;
    taxonomy_view(&taxonomy)
}

pub(crate) async fn get_node<R: CategoryRepository + ?Sized>(
    categories: &R,
    user_id: UserId,
    id: CategoryId,
    now: DateTime<Utc>,
) -> Result<CategoryNodeResult, ClassificationError> {
    let taxonomy = load_or_initialize(categories, user_id, now).await?;
    node_result(&taxonomy, id)
}

pub(crate) async fn create_node<R: CategoryRepository + ?Sized>(
    categories: &R,
    command: CreateCategoryNode,
    now: DateTime<Utc>,
) -> Result<CategoryNodeResult, ClassificationError> {
    let hash = command_hash(&json!({
        "expected_version": command.expected_version,
        "name": command.name,
        "kind": command.kind.as_str(),
        "parent_id": command.parent_id,
        "position": command.position,
        "color": command.color,
        "icon": command.icon,
    }))?;
    if let Some(response) = categories
        .find_receipt(command.user_id, "create", &command.idempotency_key, &hash)
        .await?
    {
        return deserialize_response(response);
    }
    let mut taxonomy = load_or_initialize(categories, command.user_id, now).await?;
    let expected = command.expected_version;
    let id = taxonomy.create_node(
        expected,
        command.name,
        command.kind,
        command.parent_id,
        command.position,
        command.color,
        command.icon,
        now,
    )?;
    let result = node_result(&taxonomy, id)?;
    persist_idempotent(
        categories,
        &taxonomy,
        expected,
        "create",
        &command.idempotency_key,
        &hash,
        result,
    )
    .await
}

pub(crate) async fn update_node<R: CategoryRepository + ?Sized>(
    categories: &R,
    command: UpdateCategoryNode,
    now: DateTime<Utc>,
) -> Result<CategoryNodeResult, ClassificationError> {
    let hash = command_hash(&json!({
        "id": command.id,
        "expected_version": command.expected_version,
        "name": command.name,
        "color": command.color,
        "icon": command.icon,
    }))?;
    if let Some(response) = categories
        .find_receipt(command.user_id, "update", &command.idempotency_key, &hash)
        .await?
    {
        return deserialize_response(response);
    }
    let mut taxonomy = load_or_initialize(categories, command.user_id, now).await?;
    let expected = command.expected_version;
    taxonomy.update_node(
        expected,
        command.id,
        command.name,
        command.color,
        command.icon,
        now,
    )?;
    let result = node_result(&taxonomy, command.id)?;
    persist_idempotent(
        categories,
        &taxonomy,
        expected,
        "update",
        &command.idempotency_key,
        &hash,
        result,
    )
    .await
}

pub(crate) async fn move_node<R: CategoryRepository + ?Sized>(
    categories: &R,
    command: MoveCategoryNode,
    now: DateTime<Utc>,
) -> Result<CategoryNodeResult, ClassificationError> {
    let hash = command_hash(&json!({
        "id": command.id,
        "expected_version": command.expected_version,
        "parent_id": command.parent_id,
        "target_position": command.target_position,
    }))?;
    if let Some(response) = categories
        .find_receipt(command.user_id, "move", &command.idempotency_key, &hash)
        .await?
    {
        return deserialize_response(response);
    }
    let mut taxonomy = load_or_initialize(categories, command.user_id, now).await?;
    let expected = command.expected_version;
    taxonomy.move_node(
        expected,
        command.id,
        command.parent_id,
        command.target_position,
        now,
    )?;
    let result = node_result(&taxonomy, command.id)?;
    persist_idempotent(
        categories,
        &taxonomy,
        expected,
        "move",
        &command.idempotency_key,
        &hash,
        result,
    )
    .await
}

pub(crate) async fn reorder_nodes<R: CategoryRepository + ?Sized>(
    categories: &R,
    command: ReorderCategoryNodes,
    now: DateTime<Utc>,
) -> Result<TaxonomyView, ClassificationError> {
    let hash = command_hash(&json!({
        "expected_version": command.expected_version,
        "parent_id": command.parent_id,
        "ordered_category_ids": command.ordered_category_ids,
    }))?;
    if let Some(response) = categories
        .find_receipt(command.user_id, "reorder", &command.idempotency_key, &hash)
        .await?
    {
        return deserialize_response(response);
    }
    let mut taxonomy = load_or_initialize(categories, command.user_id, now).await?;
    let expected = command.expected_version;
    taxonomy.reorder(
        expected,
        command.parent_id,
        command.ordered_category_ids,
        now,
    )?;
    let result = taxonomy_view(&taxonomy)?;
    persist_idempotent(
        categories,
        &taxonomy,
        expected,
        "reorder",
        &command.idempotency_key,
        &hash,
        result,
    )
    .await
}

pub(crate) async fn set_node_lifecycle<R: CategoryRepository + ?Sized>(
    categories: &R,
    command: SetCategoryNodeLifecycle,
    now: DateTime<Utc>,
) -> Result<CategoryNodeResult, ClassificationError> {
    let scope = match command.lifecycle {
        CategoryLifecycle::Active => "restore",
        CategoryLifecycle::Archived => "archive",
    };
    let hash = command_hash(&json!({
        "id": command.id,
        "expected_version": command.expected_version,
        "lifecycle": command.lifecycle.as_str(),
    }))?;
    if let Some(response) = categories
        .find_receipt(command.user_id, scope, &command.idempotency_key, &hash)
        .await?
    {
        return deserialize_response(response);
    }
    let mut taxonomy = load_or_initialize(categories, command.user_id, now).await?;
    taxonomy.set_lifecycle(command.expected_version, command.id, command.lifecycle, now)?;
    let result = node_result(&taxonomy, command.id)?;
    persist_idempotent(
        categories,
        &taxonomy,
        command.expected_version,
        scope,
        &command.idempotency_key,
        &hash,
        result,
    )
    .await
}

pub(crate) async fn require_assignable<R: CategoryRepository + ?Sized>(
    categories: &R,
    user_id: UserId,
    id: CategoryId,
    transaction_kind: CategoryKind,
) -> Result<CategoryView, ClassificationError> {
    let taxonomy = load_or_initialize(categories, user_id, Utc::now()).await?;
    let category = taxonomy.category(id)?;
    let compatible = category.kind() == CategoryKind::Both || category.kind() == transaction_kind;
    if !taxonomy.is_effectively_active(id)? || !taxonomy.is_leaf(id) || !compatible {
        return Err(ClassificationError::not_assignable());
    }
    category_view(&taxonomy, id)
}

pub(crate) async fn resolve_subtree<R: CategoryRepository + ?Sized>(
    categories: &R,
    user_id: UserId,
    id: CategoryId,
) -> Result<Vec<CategoryId>, ClassificationError> {
    let taxonomy = load_or_initialize(categories, user_id, Utc::now()).await?;
    taxonomy.category(id)?;
    Ok(taxonomy.subtree_ids(id))
}

async fn load_or_initialize<R: CategoryRepository + ?Sized>(
    repository: &R,
    user_id: UserId,
    now: DateTime<Utc>,
) -> Result<CategoryTaxonomy, ClassificationError> {
    if let Some(taxonomy) = repository.load_taxonomy(user_id).await? {
        return Ok(taxonomy);
    }
    let existing = repository.list_for_user(user_id).await?;
    let candidate = if existing.is_empty() {
        CategoryTaxonomy::starter(user_id, now)
    } else {
        CategoryTaxonomy::for_existing(user_id, existing, now)?
    };
    if repository.initialize_taxonomy(&candidate).await? {
        return Ok(candidate);
    }
    repository.load_taxonomy(user_id).await?.ok_or_else(|| {
        ClassificationError::persistence("category taxonomy initialization was lost")
    })
}

fn taxonomy_view(taxonomy: &CategoryTaxonomy) -> Result<TaxonomyView, ClassificationError> {
    let roots = taxonomy
        .children_ids(None)
        .into_iter()
        .map(|id| tree_node(taxonomy, id))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(TaxonomyView {
        user_id: taxonomy.user_id(),
        version: taxonomy.version(),
        starter_template_version: taxonomy.starter_template_version(),
        roots,
        as_of: taxonomy.updated_at(),
    })
}

fn node_result(
    taxonomy: &CategoryTaxonomy,
    id: CategoryId,
) -> Result<CategoryNodeResult, ClassificationError> {
    Ok(CategoryNodeResult {
        taxonomy_version: taxonomy.version(),
        starter_template_version: taxonomy.starter_template_version(),
        node: tree_node(taxonomy, id)?,
    })
}

fn tree_node(
    taxonomy: &CategoryTaxonomy,
    id: CategoryId,
) -> Result<CategoryNodeView, ClassificationError> {
    Ok(CategoryNodeView {
        category: category_view(taxonomy, id)?,
        children: taxonomy
            .children_ids(Some(id))
            .into_iter()
            .map(|child| tree_node(taxonomy, child))
            .collect::<Result<Vec<_>, _>>()?,
    })
}

fn category_view(
    taxonomy: &CategoryTaxonomy,
    id: CategoryId,
) -> Result<CategoryView, ClassificationError> {
    let category = taxonomy.category(id)?;
    let active = taxonomy.is_effectively_active(id)?;
    Ok(CategoryView {
        id: category.id(),
        user_id: category.user_id(),
        name: category.name().to_owned(),
        kind: category.kind(),
        lifecycle: category.lifecycle(),
        effective_lifecycle: if active {
            CategoryLifecycle::Active
        } else {
            CategoryLifecycle::Archived
        },
        parent_id: category.parent_id(),
        position: category.position(),
        color: category.color().map(str::to_owned),
        effective_color: taxonomy.effective_color(id)?,
        icon: category.icon().map(str::to_owned),
        effective_icon: taxonomy.effective_icon(id)?,
        depth: taxonomy.depth(id)?,
        path: taxonomy.path(id)?,
        assignable: active && taxonomy.is_leaf(id),
        version: category.version(),
        created_at: category.created_at(),
        as_of: category.updated_at(),
    })
}

fn command_hash(value: &Value) -> Result<[u8; 32], ClassificationError> {
    Ok(Sha256::digest(serde_json::to_vec(value).map_err(ClassificationError::storage)?).into())
}

fn deserialize_response<T: DeserializeOwned>(value: Value) -> Result<T, ClassificationError> {
    serde_json::from_value(value).map_err(ClassificationError::storage)
}

async fn persist_idempotent<R, T>(
    repository: &R,
    taxonomy: &CategoryTaxonomy,
    expected_version: i64,
    scope: &'static str,
    key: &IdempotencyKey,
    request_hash: &[u8; 32],
    result: T,
) -> Result<T, ClassificationError>
where
    R: CategoryRepository + ?Sized,
    T: serde::Serialize + DeserializeOwned,
{
    let response = serde_json::to_value(&result).map_err(ClassificationError::storage)?;
    if let Some(replayed) = repository
        .save_taxonomy_idempotent(
            taxonomy,
            expected_version,
            scope,
            key,
            request_hash,
            &response,
        )
        .await?
    {
        return deserialize_response(replayed);
    }
    Ok(result)
}
