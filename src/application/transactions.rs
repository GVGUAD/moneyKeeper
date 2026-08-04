use chrono::DateTime;
use chrono::Utc;
use rust_decimal::Decimal;
use std::sync::Arc;
use uuid::Uuid;

use crate::domain::account::AccountRepository;
use crate::domain::bank_connection::BankConnectionRepository;
use crate::domain::error::DomainError;
use crate::domain::transaction::{
    Transaction, TransactionDetails, TransactionKind, TransactionListParams, TransactionRepository,
};

fn signed_delta(kind: &TransactionKind, amount: Decimal) -> Decimal {
    if kind.affects_balance_positively() {
        amount
    } else {
        -amount
    }
}

pub struct TransactionService {
    repo: Arc<dyn TransactionRepository>,
    account_repo: Arc<dyn AccountRepository>,
    connection_repo: Arc<dyn BankConnectionRepository>,
}

impl TransactionService {
    pub fn new(
        repo: Arc<dyn TransactionRepository>,
        account_repo: Arc<dyn AccountRepository>,
        connection_repo: Arc<dyn BankConnectionRepository>,
    ) -> Self {
        Self {
            repo,
            account_repo,
            connection_repo,
        }
    }

    async fn ensure_not_bank_linked(&self, account_id: Uuid) -> anyhow::Result<()> {
        if self.connection_repo.exists_for_account(account_id).await? {
            return Err(DomainError::Conflict(
                "account is bank-linked; manual transactions are not allowed".to_string(),
            )
            .into());
        }
        Ok(())
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
        self.ensure_not_bank_linked(account_id).await?;
        let tx = Transaction::new(
            account_id,
            user_id,
            amount,
            currency,
            kind.clone(),
            category_id,
            note,
            transacted_at,
        );
        self.repo.create(&tx, &details).await?;
        self.account_repo
            .adjust_balance(account_id, user_id, signed_delta(&kind, amount))
            .await?;
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

    pub async fn count(&self, params: TransactionListParams) -> anyhow::Result<i64> {
        self.repo.count(&params).await
    }

    pub async fn delete(&self, id: Uuid, user_id: Uuid) -> anyhow::Result<()> {
        let (tx, _) = self.get(id, user_id).await?;
        self.ensure_not_bank_linked(tx.account_id).await?;
        self.repo.delete(id, user_id).await?;
        self.account_repo
            .adjust_balance(
                tx.account_id,
                tx.user_id,
                -signed_delta(&tx.kind, tx.amount),
            )
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::account::{Account, AccountDetails};
    use crate::domain::bank_connection::{BankConnection, BankProvider, SyncStatus};
    use chrono::DateTime;
    use std::sync::Mutex;

    fn assert_params_eq(a: &TransactionListParams, b: &TransactionListParams) {
        assert_eq!(a.account_id, b.account_id);
        assert_eq!(a.user_id, b.user_id);
        assert_eq!(a.kind, b.kind);
        assert_eq!(a.category_id, b.category_id);
        assert_eq!(a.from, b.from);
        assert_eq!(a.to, b.to);
        assert_eq!(a.limit, b.limit);
        assert_eq!(a.offset, b.offset);
    }

    fn sample_params() -> TransactionListParams {
        TransactionListParams {
            account_id: Some(Uuid::new_v4()),
            user_id: Uuid::new_v4(),
            kind: Some(TransactionKind::Income),
            category_id: Some(Uuid::new_v4()),
            from: Some(Utc::now()),
            to: Some(Utc::now()),
            limit: 25,
            offset: 7,
        }
    }

    #[derive(Default)]
    struct MockTxRepo {
        last_list: Mutex<Option<TransactionListParams>>,
        last_count: Mutex<Option<TransactionListParams>>,
        list_return: Mutex<Vec<(Transaction, TransactionDetails)>>,
        count_return: Mutex<i64>,
    }

    #[async_trait::async_trait]
    impl TransactionRepository for MockTxRepo {
        async fn create(
            &self,
            _tx: &Transaction,
            _details: &TransactionDetails,
        ) -> anyhow::Result<()> {
            unimplemented!()
        }
        async fn find_by_id(
            &self,
            _id: Uuid,
            _user_id: Uuid,
        ) -> anyhow::Result<Option<(Transaction, TransactionDetails)>> {
            unimplemented!()
        }
        async fn list(
            &self,
            params: &TransactionListParams,
        ) -> anyhow::Result<Vec<(Transaction, TransactionDetails)>> {
            *self.last_list.lock().unwrap() = Some(params.clone());
            let mut out = Vec::new();
            std::mem::swap(&mut *self.list_return.lock().unwrap(), &mut out);
            Ok(out)
        }
        async fn count(&self, params: &TransactionListParams) -> anyhow::Result<i64> {
            *self.last_count.lock().unwrap() = Some(params.clone());
            Ok(*self.count_return.lock().unwrap())
        }
        async fn update(
            &self,
            _tx: &Transaction,
            _details: &TransactionDetails,
        ) -> anyhow::Result<()> {
            unimplemented!()
        }
        async fn delete(&self, _id: Uuid, _user_id: Uuid) -> anyhow::Result<()> {
            unimplemented!()
        }
        async fn create_idempotent(&self, _tx: &Transaction) -> anyhow::Result<bool> {
            unimplemented!()
        }
        async fn list_match_candidates(
            &self,
            _user_id: Uuid,
            _from: chrono::DateTime<chrono::Utc>,
            _to: chrono::DateTime<chrono::Utc>,
            _min_amount: rust_decimal::Decimal,
            _max_amount: rust_decimal::Decimal,
            _currency: &str,
        ) -> anyhow::Result<Vec<Transaction>> {
            unimplemented!()
        }
    }

    struct StubAccountRepo;

    #[async_trait::async_trait]
    impl AccountRepository for StubAccountRepo {
        async fn create(
            &self,
            _account: &Account,
            _details: &AccountDetails,
        ) -> anyhow::Result<()> {
            unimplemented!()
        }
        async fn find_by_id(
            &self,
            _id: Uuid,
            _user_id: Uuid,
        ) -> anyhow::Result<Option<(Account, AccountDetails)>> {
            unimplemented!()
        }
        async fn list_by_user(
            &self,
            _user_id: Uuid,
        ) -> anyhow::Result<Vec<(Account, AccountDetails)>> {
            unimplemented!()
        }
        async fn update(
            &self,
            _account: &Account,
            _details: &AccountDetails,
        ) -> anyhow::Result<()> {
            unimplemented!()
        }
        async fn delete(&self, _id: Uuid, _user_id: Uuid) -> anyhow::Result<()> {
            unimplemented!()
        }
        async fn adjust_balance(
            &self,
            _account_id: Uuid,
            _user_id: Uuid,
            _delta: Decimal,
        ) -> anyhow::Result<()> {
            unimplemented!()
        }
        async fn set_balance(
            &self,
            _account_id: Uuid,
            _user_id: Uuid,
            _balance: Decimal,
        ) -> anyhow::Result<()> {
            unimplemented!()
        }
        async fn sync_balance_from_external(
            &self,
            _account_id: Uuid,
            _user_id: Uuid,
        ) -> anyhow::Result<()> {
            unimplemented!()
        }
    }

    #[derive(Default)]
    struct StubConnectionRepo {
        bank_linked_accounts: Mutex<Vec<Uuid>>,
    }

    #[async_trait::async_trait]
    impl crate::domain::bank_connection::BankConnectionRepository for StubConnectionRepo {
        async fn create(&self, _conn: &BankConnection) -> anyhow::Result<()> {
            unimplemented!()
        }
        async fn find_by_id(
            &self,
            _id: Uuid,
            _user_id: Uuid,
        ) -> anyhow::Result<Option<BankConnection>> {
            unimplemented!()
        }
        async fn find_by_external_account_id(
            &self,
            _provider: &BankProvider,
            _external_account_id: &str,
        ) -> anyhow::Result<Option<BankConnection>> {
            unimplemented!()
        }
        async fn list_by_user(&self, _user_id: Uuid) -> anyhow::Result<Vec<BankConnection>> {
            unimplemented!()
        }
        async fn list_incomplete(&self) -> anyhow::Result<Vec<BankConnection>> {
            unimplemented!()
        }
        async fn update_status(
            &self,
            _id: Uuid,
            _status: SyncStatus,
            _last_synced_at: Option<DateTime<Utc>>,
        ) -> anyhow::Result<()> {
            unimplemented!()
        }
        async fn delete(&self, _id: Uuid, _user_id: Uuid) -> anyhow::Result<()> {
            unimplemented!()
        }
        async fn exists_for_account(&self, account_id: Uuid) -> anyhow::Result<bool> {
            Ok(self
                .bank_linked_accounts
                .lock()
                .unwrap()
                .contains(&account_id))
        }
    }

    #[tokio::test]
    async fn list_forwards_params_to_repo() {
        let repo = Arc::new(MockTxRepo::default());
        let canned = vec![(
            Transaction::new(
                Uuid::new_v4(),
                Uuid::new_v4(),
                Decimal::new(100, 0),
                "USD".to_string(),
                TransactionKind::Income,
                None,
                None,
                Utc::now(),
            ),
            TransactionDetails::None,
        )];
        let canned_id = canned[0].0.id;
        *repo.list_return.lock().unwrap() = canned;

        let svc = TransactionService::new(
            repo.clone(),
            Arc::new(StubAccountRepo),
            Arc::new(StubConnectionRepo::default()),
        );
        let params = sample_params();
        let result = svc.list(params.clone()).await.unwrap();

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].0.id, canned_id);
        let recorded = repo.last_list.lock().unwrap().clone().expect("list called");
        assert_params_eq(&recorded, &params);
    }

    #[tokio::test]
    async fn count_forwards_params_to_repo() {
        let repo = Arc::new(MockTxRepo::default());
        *repo.count_return.lock().unwrap() = 42;

        let svc = TransactionService::new(
            repo.clone(),
            Arc::new(StubAccountRepo),
            Arc::new(StubConnectionRepo::default()),
        );
        let params = sample_params();
        let result = svc.count(params.clone()).await.unwrap();

        assert_eq!(result, 42);
        let recorded = repo
            .last_count
            .lock()
            .unwrap()
            .clone()
            .expect("count called");
        assert_params_eq(&recorded, &params);
    }
}
