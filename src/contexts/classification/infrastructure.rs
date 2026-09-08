use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::shared_kernel::{IdempotencyKey, UserId};

use super::application::CategoryRepository;
use super::domain::{Category, CategoryTaxonomy};
use super::public::{CategoryId, CategoryKind, CategoryLifecycle, ClassificationError};

#[derive(sqlx::FromRow)]
struct CategoryRow {
    id: Uuid,
    user_id: Uuid,
    name: String,
    kind: String,
    lifecycle: String,
    parent_id: Option<Uuid>,
    position: i32,
    color: Option<String>,
    icon: Option<String>,
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
            self.parent_id.map(CategoryId::new),
            self.position,
            self.color,
            self.icon,
            self.version,
            self.created_at,
            self.updated_at,
        )
    }
}

#[derive(sqlx::FromRow)]
struct TaxonomyRow {
    user_id: Uuid,
    version: i64,
    starter_template_version: Option<i32>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

#[derive(Clone)]
pub(crate) struct PgCategoryCatalog {
    pool: PgPool,
}

impl PgCategoryCatalog {
    pub(crate) fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

fn database_error(source: sqlx::Error) -> ClassificationError {
    let duplicate = source.as_database_error().is_some_and(|error| {
        error.code().as_deref() == Some("23505")
            && matches!(
                error.constraint(),
                Some("categories_active_name_unique" | "categories_active_sibling_name_unique")
            )
    });
    if duplicate {
        ClassificationError::duplicate_name(source)
    } else {
        ClassificationError::storage(source)
    }
}

const CATEGORY_COLUMNS: &str = "id, user_id, name, kind, lifecycle, parent_id, position, color, icon, version, created_at, updated_at";

#[async_trait]
impl CategoryRepository for PgCategoryCatalog {
    async fn lock_taxonomy(
        &self,
        user_id: UserId,
    ) -> Result<super::public::CategoryTaxonomyGuard, ClassificationError> {
        let mut tx = self.pool.begin().await.map_err(database_error)?;
        let row = sqlx::query_as::<_, TaxonomyRow>(
            "SELECT user_id,version,starter_template_version,created_at,updated_at \
             FROM classification.category_taxonomies WHERE user_id=$1 FOR SHARE",
        )
        .bind(user_id.into_uuid())
        .fetch_optional(&mut *tx)
        .await
        .map_err(database_error)?
        .ok_or_else(ClassificationError::not_found)?;
        let sql = format!(
            "SELECT {CATEGORY_COLUMNS} FROM classification.categories WHERE user_id=$1 ORDER BY parent_id NULLS FIRST,position,id"
        );
        let nodes = sqlx::query_as::<_, CategoryRow>(&sql)
            .bind(user_id.into_uuid())
            .fetch_all(&mut *tx)
            .await
            .map_err(database_error)?
            .into_iter()
            .map(CategoryRow::into_domain)
            .collect::<Result<Vec<_>, _>>()?;
        let taxonomy = CategoryTaxonomy::reconstitute(
            user_id,
            row.version,
            row.starter_template_version,
            nodes,
            row.created_at,
            row.updated_at,
        )?;
        Ok(super::public::CategoryTaxonomyGuard {
            taxonomy,
            _lease: Box::new(tx),
        })
    }

    async fn list_for_user(&self, user_id: UserId) -> Result<Vec<Category>, ClassificationError> {
        load_categories(&self.pool, user_id).await
    }

    async fn load_taxonomy(
        &self,
        user_id: UserId,
    ) -> Result<Option<CategoryTaxonomy>, ClassificationError> {
        let mut tx = self.pool.begin().await.map_err(database_error)?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
            .execute(&mut *tx)
            .await
            .map_err(database_error)?;
        let row = sqlx::query_as::<_, TaxonomyRow>(
            "SELECT user_id,version,starter_template_version,created_at,updated_at \
             FROM classification.category_taxonomies WHERE user_id=$1",
        )
        .bind(user_id.into_uuid())
        .fetch_optional(&mut *tx)
        .await
        .map_err(database_error)?;
        let Some(row) = row else {
            return Ok(None);
        };
        let sql = format!(
            "SELECT {CATEGORY_COLUMNS} FROM classification.categories WHERE user_id=$1 ORDER BY parent_id NULLS FIRST,position,id"
        );
        let values = sqlx::query_as::<_, CategoryRow>(&sql)
            .bind(user_id.into_uuid())
            .fetch_all(&mut *tx)
            .await
            .map_err(database_error)?
            .into_iter()
            .map(CategoryRow::into_domain)
            .collect::<Result<Vec<_>, _>>()?;
        tx.commit().await.map_err(database_error)?;
        CategoryTaxonomy::reconstitute(
            UserId::new(row.user_id),
            row.version,
            row.starter_template_version,
            values,
            row.created_at,
            row.updated_at,
        )
        .map(Some)
    }

    async fn initialize_taxonomy(
        &self,
        taxonomy: &CategoryTaxonomy,
    ) -> Result<bool, ClassificationError> {
        let mut tx = self.pool.begin().await.map_err(database_error)?;
        tenant_lock(&mut tx, taxonomy.user_id()).await?;
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM classification.category_taxonomies WHERE user_id=$1)",
        )
        .bind(taxonomy.user_id().into_uuid())
        .fetch_one(&mut *tx)
        .await
        .map_err(database_error)?;
        if exists {
            tx.rollback().await.map_err(database_error)?;
            return Ok(false);
        }
        let has_categories: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM classification.categories WHERE user_id=$1)",
        )
        .bind(taxonomy.user_id().into_uuid())
        .fetch_one(&mut *tx)
        .await
        .map_err(database_error)?;
        let template_version = if has_categories {
            None
        } else {
            taxonomy.starter_template_version()
        };
        sqlx::query(
            "INSERT INTO classification.category_taxonomies(user_id,version,starter_template_version,created_at,updated_at) VALUES($1,1,$2,$3,$4)",
        )
        .bind(taxonomy.user_id().into_uuid())
        .bind(template_version)
        .bind(taxonomy.created_at())
        .bind(taxonomy.updated_at())
        .execute(&mut *tx)
        .await
        .map_err(database_error)?;
        if !has_categories {
            sqlx::query("SET CONSTRAINTS classification.categories_sibling_position_unique, classification.categories_tenant_parent_fk DEFERRED")
                .execute(&mut *tx).await.map_err(database_error)?;
            for category in taxonomy.categories() {
                insert_category(&mut tx, category).await?;
            }
        }
        tx.commit().await.map_err(database_error)?;
        Ok(!has_categories)
    }

    async fn find_receipt(
        &self,
        user_id: UserId,
        scope: &'static str,
        key: &IdempotencyKey,
        request_hash: &[u8; 32],
    ) -> Result<Option<Value>, ClassificationError> {
        let row = sqlx::query_as::<_, (Vec<u8>, Value)>(
            "SELECT request_hash,response_body FROM classification.category_command_receipts \
             WHERE user_id=$1 AND command_scope=$2 AND idempotency_key=$3",
        )
        .bind(user_id.into_uuid())
        .bind(scope)
        .bind(key.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(database_error)?;
        match row {
            Some((stored_hash, response)) if stored_hash.as_slice() == request_hash => {
                Ok(Some(response))
            }
            Some(_) => Err(ClassificationError::idempotency_conflict()),
            None => Ok(None),
        }
    }

    async fn save_taxonomy_idempotent(
        &self,
        taxonomy: &CategoryTaxonomy,
        expected_version: i64,
        scope: &'static str,
        key: &IdempotencyKey,
        request_hash: &[u8; 32],
        response: &Value,
    ) -> Result<Option<Value>, ClassificationError> {
        let mut tx = self.pool.begin().await.map_err(database_error)?;
        command_lock(&mut tx, taxonomy.user_id(), scope, key).await?;
        let existing = sqlx::query_as::<_, (Vec<u8>, Value)>(
            "SELECT request_hash,response_body FROM classification.category_command_receipts \
             WHERE user_id=$1 AND command_scope=$2 AND idempotency_key=$3 FOR UPDATE",
        )
        .bind(taxonomy.user_id().into_uuid())
        .bind(scope)
        .bind(key.as_str())
        .fetch_optional(&mut *tx)
        .await
        .map_err(database_error)?;
        if let Some((stored_hash, stored_response)) = existing {
            if stored_hash.as_slice() != request_hash {
                return Err(ClassificationError::idempotency_conflict());
            }
            tx.rollback().await.map_err(database_error)?;
            return Ok(Some(stored_response));
        }
        save_aggregate(&mut tx, taxonomy, expected_version).await?;
        sqlx::query(
            "INSERT INTO classification.category_command_receipts(user_id,command_scope,idempotency_key,request_hash,response_body) VALUES($1,$2,$3,$4,$5)",
        )
        .bind(taxonomy.user_id().into_uuid())
        .bind(scope)
        .bind(key.as_str())
        .bind(request_hash.as_slice())
        .bind(response)
        .execute(&mut *tx)
        .await
        .map_err(database_error)?;
        tx.commit().await.map_err(database_error)?;
        Ok(None)
    }
}

async fn load_categories(
    pool: &PgPool,
    user_id: UserId,
) -> Result<Vec<Category>, ClassificationError> {
    let sql = format!(
        "SELECT {CATEGORY_COLUMNS} FROM classification.categories WHERE user_id=$1 ORDER BY parent_id NULLS FIRST,position,id"
    );
    sqlx::query_as::<_, CategoryRow>(&sql)
        .bind(user_id.into_uuid())
        .fetch_all(pool)
        .await
        .map_err(database_error)?
        .into_iter()
        .map(CategoryRow::into_domain)
        .collect()
}

async fn tenant_lock(
    tx: &mut Transaction<'_, Postgres>,
    user_id: UserId,
) -> Result<(), ClassificationError> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1::text, 0))")
        .bind(user_id.into_uuid())
        .execute(&mut **tx)
        .await
        .map_err(database_error)?;
    Ok(())
}

async fn command_lock(
    tx: &mut Transaction<'_, Postgres>,
    user_id: UserId,
    scope: &str,
    key: &IdempotencyKey,
) -> Result<(), ClassificationError> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(format!("{}:{scope}:{}", user_id, key.as_str()))
        .execute(&mut **tx)
        .await
        .map_err(database_error)?;
    Ok(())
}

async fn save_aggregate(
    tx: &mut Transaction<'_, Postgres>,
    taxonomy: &CategoryTaxonomy,
    expected_version: i64,
) -> Result<(), ClassificationError> {
    sqlx::query(
        "SET CONSTRAINTS classification.categories_sibling_position_unique, classification.categories_tenant_parent_fk DEFERRED",
    )
    .execute(&mut **tx)
    .await
    .map_err(database_error)?;
    let updated = sqlx::query(
        "UPDATE classification.category_taxonomies SET version=$1,starter_template_version=$2,updated_at=$3 WHERE user_id=$4 AND version=$5",
    )
    .bind(taxonomy.version())
    .bind(taxonomy.starter_template_version())
    .bind(taxonomy.updated_at())
    .bind(taxonomy.user_id().into_uuid())
    .bind(expected_version)
    .execute(&mut **tx)
    .await
    .map_err(database_error)?;
    if updated.rows_affected() == 0 {
        return Err(ClassificationError::version_conflict());
    }
    for category in taxonomy.categories() {
        sqlx::query(
            "INSERT INTO classification.categories \
             (id,user_id,name,kind,lifecycle,parent_id,position,color,icon,version,created_at,updated_at) \
             VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12) \
             ON CONFLICT (id,user_id) DO UPDATE SET name=EXCLUDED.name,kind=EXCLUDED.kind,lifecycle=EXCLUDED.lifecycle,parent_id=EXCLUDED.parent_id,position=EXCLUDED.position,color=EXCLUDED.color,icon=EXCLUDED.icon,version=EXCLUDED.version,updated_at=EXCLUDED.updated_at",
        )
        .bind(category.id().into_uuid())
        .bind(category.user_id().into_uuid())
        .bind(category.name())
        .bind(category.kind().as_str())
        .bind(category.lifecycle().as_str())
        .bind(category.parent_id().map(CategoryId::into_uuid))
        .bind(category.position())
        .bind(category.color())
        .bind(category.icon())
        .bind(category.version())
        .bind(category.created_at())
        .bind(category.updated_at())
        .execute(&mut **tx)
        .await
        .map_err(database_error)?;
    }
    // The taxonomy version is part of every classifier generation. Fence all
    // unfinished work in the same Classification transaction; the integration
    // intake worker rebuilds minimized evidence against the new tree.
    sqlx::query(
        "UPDATE classification.classification_targets target SET \
           state='stale',lease_holder=NULL,lease_expires_at=NULL,updated_at=$2 \
         WHERE target.user_id=$1 AND target.state IN ( \
           'pending','retry_due','quota_deferred','auto_apply_pending','review_pending','applying')",
    )
    .bind(taxonomy.user_id().into_uuid())
    .bind(taxonomy.updated_at())
    .execute(&mut **tx)
    .await
    .map_err(database_error)?;
    sqlx::query(
        "UPDATE classification.classification_decisions decision SET \
           state='stale',version=version+1,updated_at=$2 \
         WHERE decision.user_id=$1 \
           AND decision.state IN ('auto_apply_pending','review_pending','applying')",
    )
    .bind(taxonomy.user_id().into_uuid())
    .bind(taxonomy.updated_at())
    .execute(&mut **tx)
    .await
    .map_err(database_error)?;
    Ok(())
}

async fn insert_category(
    tx: &mut Transaction<'_, Postgres>,
    category: &Category,
) -> Result<(), ClassificationError> {
    sqlx::query(
        "INSERT INTO classification.categories(id,user_id,name,kind,lifecycle,parent_id,position,color,icon,version,created_at,updated_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12)",
    )
    .bind(category.id().into_uuid())
    .bind(category.user_id().into_uuid())
    .bind(category.name())
    .bind(category.kind().as_str())
    .bind(category.lifecycle().as_str())
    .bind(category.parent_id().map(CategoryId::into_uuid))
    .bind(category.position())
    .bind(category.color())
    .bind(category.icon())
    .bind(category.version())
    .bind(category.created_at())
    .bind(category.updated_at())
    .execute(&mut **tx)
    .await
    .map_err(database_error)?;
    Ok(())
}
