use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use crate::domain::category::{Category, CategoryRepository};

pub struct SqliteCategoryRepository {
    pool: PgPool,
}

impl SqliteCategoryRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[derive(sqlx::FromRow)]
struct CategoryRow {
    id: Uuid,
    user_id: Uuid,
    name: String,
    color: Option<String>,
    created_at: DateTime<Utc>,
}

fn row_to_category(r: CategoryRow) -> Category {
    Category {
        id: r.id,
        user_id: r.user_id,
        name: r.name,
        color: r.color,
        created_at: r.created_at,
    }
}

#[async_trait::async_trait]
impl CategoryRepository for SqliteCategoryRepository {
    async fn create(&self, c: &Category) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO categories (id, user_id, name, color, created_at) \
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(c.id)
        .bind(c.user_id)
        .bind(&c.name)
        .bind(&c.color)
        .bind(c.created_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn list_by_user(&self, user_id: Uuid) -> anyhow::Result<Vec<Category>> {
        let rows = sqlx::query_as::<_, CategoryRow>(
            "SELECT * FROM categories WHERE user_id = $1 ORDER BY name",
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(row_to_category).collect())
    }

    async fn find_by_id(&self, id: Uuid, user_id: Uuid) -> anyhow::Result<Option<Category>> {
        let row = sqlx::query_as::<_, CategoryRow>(
            "SELECT * FROM categories WHERE id = $1 AND user_id = $2",
        )
        .bind(id)
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(row_to_category))
    }

    async fn update(&self, c: &Category) -> anyhow::Result<()> {
        sqlx::query("UPDATE categories SET name = $1, color = $2 WHERE id = $3 AND user_id = $4")
            .bind(&c.name)
            .bind(&c.color)
            .bind(c.id)
            .bind(c.user_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn delete(&self, id: Uuid, user_id: Uuid) -> anyhow::Result<()> {
        sqlx::query("DELETE FROM categories WHERE id = $1 AND user_id = $2")
            .bind(id)
            .bind(user_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::PgPool;

    #[sqlx::test(migrations = "src/infrastructure/migrations")]
    async fn create_list_delete_category(pool: PgPool) {
        let repo = SqliteCategoryRepository::new(pool);
        let user_id = Uuid::new_v4();
        let cat = Category::new(user_id, "Food".to_string(), Some("#ff0000".to_string()));
        repo.create(&cat).await.unwrap();
        let list = repo.list_by_user(user_id).await.unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "Food");
        repo.delete(cat.id, user_id).await.unwrap();
        let list = repo.list_by_user(user_id).await.unwrap();
        assert!(list.is_empty());
    }

    #[sqlx::test(migrations = "src/infrastructure/migrations")]
    async fn update_category_name(pool: PgPool) {
        let repo = SqliteCategoryRepository::new(pool);
        let user_id = Uuid::new_v4();
        let mut cat = Category::new(user_id, "Food".to_string(), None);
        repo.create(&cat).await.unwrap();
        cat.name = "Groceries".to_string();
        repo.update(&cat).await.unwrap();
        let found = repo.find_by_id(cat.id, user_id).await.unwrap().unwrap();
        assert_eq!(found.name, "Groceries");
    }
}
