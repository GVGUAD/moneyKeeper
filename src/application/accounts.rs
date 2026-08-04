use chrono::Utc;
use std::sync::Arc;
use uuid::Uuid;

use crate::domain::account::{Account, AccountDetails, AccountRepository, AccountType};
use crate::domain::error::DomainError;

pub struct AccountService {
    repo: Arc<dyn AccountRepository>,
}

impl AccountService {
    pub fn new(repo: Arc<dyn AccountRepository>) -> Self {
        Self { repo }
    }

    pub async fn create(
        &self,
        user_id: Uuid,
        name: String,
        account_type: AccountType,
        currency: String,
        details: AccountDetails,
    ) -> anyhow::Result<Account> {
        let account = Account::new(user_id, name, account_type, currency);
        self.repo.create(&account, &details).await?;
        Ok(account)
    }

    pub async fn get(&self, id: Uuid, user_id: Uuid) -> anyhow::Result<(Account, AccountDetails)> {
        self.repo
            .find_by_id(id, user_id)
            .await?
            .ok_or_else(|| DomainError::NotFound(format!("account {id}")).into())
    }

    pub async fn list(&self, user_id: Uuid) -> anyhow::Result<Vec<(Account, AccountDetails)>> {
        self.repo.list_by_user(user_id).await
    }

    pub async fn update(
        &self,
        id: Uuid,
        user_id: Uuid,
        name: Option<String>,
        currency: Option<String>,
        details: Option<AccountDetails>,
    ) -> anyhow::Result<(Account, AccountDetails)> {
        let (mut account, existing_details) = self.get(id, user_id).await?;
        if let Some(n) = name {
            account.name = n;
        }
        if let Some(c) = currency {
            account.currency = c;
        }
        account.updated_at = Utc::now();
        let new_details = details.unwrap_or(existing_details);
        self.repo.update(&account, &new_details).await?;
        Ok((account, new_details))
    }

    pub async fn delete(&self, id: Uuid, user_id: Uuid) -> anyhow::Result<()> {
        self.get(id, user_id).await?;
        self.repo.delete(id, user_id).await
    }
}
