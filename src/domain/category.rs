use chrono::{DateTime, Utc};
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct Category {
    pub id: Uuid,
    pub user_id: Uuid,
    pub name: String,
    pub color: Option<String>,
    pub created_at: DateTime<Utc>,
}

impl Category {
    pub fn new(user_id: Uuid, name: String, color: Option<String>) -> Self {
        Self {
            id: Uuid::new_v4(),
            user_id,
            name,
            color,
            created_at: Utc::now(),
        }
    }
}

#[async_trait::async_trait]
pub trait CategoryRepository: Send + Sync {
    async fn create(&self, category: &Category) -> anyhow::Result<()>;
    async fn list_by_user(&self, user_id: Uuid) -> anyhow::Result<Vec<Category>>;
    async fn find_by_id(&self, id: Uuid, user_id: Uuid) -> anyhow::Result<Option<Category>>;
    async fn update(&self, category: &Category) -> anyhow::Result<()>;
    async fn delete(&self, id: Uuid, user_id: Uuid) -> anyhow::Result<()>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn category_new_sets_fields() {
        let user_id = uuid::Uuid::new_v4();
        let cat = Category::new(user_id, "Food".to_string(), Some("#ff0000".to_string()));
        assert_eq!(cat.user_id, user_id);
        assert_eq!(cat.name, "Food");
        assert_eq!(cat.color, Some("#ff0000".to_string()));
    }
}
