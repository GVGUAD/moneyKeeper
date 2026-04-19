use chrono::DateTime;
use chrono::Utc;
use rust_decimal::Decimal;
use std::sync::Arc;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::transaction::{
    Transaction, TransactionDetails, TransactionKind, TransactionListParams, TransactionRepository,
};

pub struct TransactionService {
    repo: Arc<dyn TransactionRepository>,
}

impl TransactionService {
    pub fn new(repo: Arc<dyn TransactionRepository>) -> Self {
        Self { repo }
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn create(
        &self,
        account_id: Uuid,
        user_id: Uuid,
        amount: Decimal,
        currency: String,
        kind: TransactionKind,
        category_id: Option<Uuid>,
        note: Option<String>,
        transacted_at: DateTime<Utc>,
        details: TransactionDetails,
    ) -> anyhow::Result<Transaction> {
        if amount <= Decimal::ZERO {
            return Err(DomainError::InvalidInput("amount must be positive".to_string()).into());
        }
        let tx = Transaction::new(
            account_id,
            user_id,
            amount,
            currency,
            kind,
            category_id,
            note,
            transacted_at,
        );
        self.repo.create(&tx, &details).await?;
        Ok(tx)
    }

    pub async fn get(
        &self,
        id: Uuid,
        user_id: Uuid,
    ) -> anyhow::Result<(Transaction, TransactionDetails)> {
        self.repo
            .find_by_id(id, user_id)
            .await?
            .ok_or_else(|| DomainError::NotFound(format!("transaction {id}")).into())
    }

    pub async fn list(
        &self,
        params: TransactionListParams,
    ) -> anyhow::Result<Vec<(Transaction, TransactionDetails)>> {
        self.repo.list(&params).await
    }

    pub async fn delete(&self, id: Uuid, user_id: Uuid) -> anyhow::Result<()> {
        self.get(id, user_id).await?;
        self.repo.delete(id, user_id).await
    }
}
