use std::sync::Arc;
use uuid::Uuid;

use crate::domain::category::{Category, CategoryRepository};
use crate::domain::error::DomainError;

pub struct CategoryService {
    repo: Arc<dyn CategoryRepository>,
}

impl CategoryService {
    pub fn new(repo: Arc<dyn CategoryRepository>) -> Self {
        Self { repo }
    }

    pub async fn create(
        &self,
        user_id: Uuid,
        name: String,
        color: Option<String>,
    ) -> anyhow::Result<Category> {
        let cat = Category::new(user_id, name, color);
        self.repo.create(&cat).await?;
        Ok(cat)
    }

    pub async fn list(&self, user_id: Uuid) -> anyhow::Result<Vec<Category>> {
        self.repo.list_by_user(user_id).await
    }

    pub async fn update(
        &self,
        id: Uuid,
        user_id: Uuid,
        name: Option<String>,
        color: Option<Option<String>>,
    ) -> anyhow::Result<Category> {
        let mut cat = self
            .repo
            .find_by_id(id, user_id)
            .await?
            .ok_or_else(|| DomainError::NotFound(format!("category {id}")))?;
        if let Some(n) = name {
            cat.name = n;
        }
        if let Some(c) = color {
            cat.color = c;
        }
        self.repo.update(&cat).await?;
        Ok(cat)
    }

    pub async fn delete(&self, id: Uuid, user_id: Uuid) -> anyhow::Result<()> {
        self.repo
            .find_by_id(id, user_id)
            .await?
            .ok_or_else(|| DomainError::NotFound(format!("category {id}")))?;
        self.repo.delete(id, user_id).await
    }
}
