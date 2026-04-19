use std::sync::Arc;

use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::monobank::{
    MonoAccount, MonoStatementItem, MonobankApiClient, MonobankConnection,
    MonobankConnectionRepository, SyncStatus,
};
use crate::domain::transaction::{Transaction, TransactionKind, TransactionRepository};

pub struct MonobankService {
    connection_repo: Arc<dyn MonobankConnectionRepository>,
    transaction_repo: Arc<dyn TransactionRepository>,
    monobank_client: Arc<dyn MonobankApiClient>,
    public_url: String,
}

impl MonobankService {
    pub fn new(
        connection_repo: Arc<dyn MonobankConnectionRepository>,
        transaction_repo: Arc<dyn TransactionRepository>,
        monobank_client: Arc<dyn MonobankApiClient>,
        public_url: String,
    ) -> Self {
        Self {
            connection_repo,
            transaction_repo,
            monobank_client,
            public_url,
        }
    }

    pub async fn get_monobank_accounts(&self, token: &str) -> anyhow::Result<Vec<MonoAccount>> {
        self.monobank_client.get_accounts(token).await
    }

    pub async fn connect(
        &self,
        account_id: Uuid,
        user_id: Uuid,
        token: String,
        monobank_account_id: String,
        account_created_at: DateTime<Utc>,
    ) -> anyhow::Result<MonobankConnection> {
        let conn = MonobankConnection::new(account_id, user_id, token, monobank_account_id);
        self.connection_repo.create(&conn).await?;
        self.spawn_sync(conn.clone(), account_created_at);
        Ok(conn)
    }

    pub async fn list_connections(&self, user_id: Uuid) -> anyhow::Result<Vec<MonobankConnection>> {
        self.connection_repo.list_by_user(user_id).await
    }

    pub async fn delete_connection(&self, id: Uuid, user_id: Uuid) -> anyhow::Result<()> {
        self.connection_repo
            .find_by_id(id, user_id)
            .await?
            .ok_or_else(|| DomainError::NotFound(format!("monobank connection {id}")))?;
        self.connection_repo.delete(id, user_id).await
    }

    pub async fn handle_webhook(
        &self,
        monobank_account_id: &str,
        item: &MonoStatementItem,
    ) -> anyhow::Result<usize> {
        let maybe_conn = self
            .connection_repo
            .find_by_monobank_account_id(monobank_account_id)
            .await?;

        match maybe_conn {
            None => {
                tracing::warn!(
                    monobank_account_id,
                    "received webhook for unknown monobank account"
                );
                Ok(0)
            }
            Some(conn) => {
                self.insert_statement_item(&conn, item).await?;
                Ok(1)
            }
        }
    }

    pub async fn restart_incomplete_syncs(&self) {
        let connections = match self.connection_repo.list_incomplete().await {
            Ok(c) => c,
            Err(e) => {
                tracing::error!("failed to list incomplete monobank connections: {e}");
                return;
            }
        };

        for conn in connections {
            if let Err(e) = self
                .connection_repo
                .update_status(conn.id, SyncStatus::Pending, None)
                .await
            {
                tracing::error!(conn_id = %conn.id, "failed to reset sync status: {e}");
                continue;
            }
            let history_from = conn.last_synced_at.unwrap_or(conn.created_at);
            self.spawn_sync(conn, history_from);
        }
    }

    pub fn spawn_sync(&self, conn: MonobankConnection, from: DateTime<Utc>) {
        let connection_repo = Arc::clone(&self.connection_repo);
        let transaction_repo = Arc::clone(&self.transaction_repo);
        let monobank_client = Arc::clone(&self.monobank_client);
        let public_url = self.public_url.clone();

        tokio::spawn(async move {
            run_sync(
                connection_repo,
                transaction_repo,
                monobank_client,
                conn,
                from,
                public_url,
            )
            .await;
        });
    }

    async fn insert_statement_item(
        &self,
        conn: &MonobankConnection,
        item: &MonoStatementItem,
    ) -> anyhow::Result<bool> {
        let tx = build_transaction(conn.account_id, conn.user_id, item);
        self.transaction_repo.create_idempotent(&tx).await?;
        Ok(true)
    }
}

fn build_transaction(
    conn_account_id: uuid::Uuid,
    conn_user_id: uuid::Uuid,
    item: &MonoStatementItem,
) -> Transaction {
    let kind = if item.is_income() {
        TransactionKind::Income
    } else {
        TransactionKind::Expense
    };
    let mut tx = Transaction::new(
        conn_account_id,
        conn_user_id,
        item.amount_decimal(),
        "UAH".to_string(),
        kind,
        None,
        item.description.clone(),
        item.transacted_at(),
    );
    tx.external_id = Some(item.id.clone());
    tx
}

async fn run_sync(
    connection_repo: Arc<dyn MonobankConnectionRepository>,
    transaction_repo: Arc<dyn TransactionRepository>,
    monobank_client: Arc<dyn MonobankApiClient>,
    conn: MonobankConnection,
    history_from: DateTime<Utc>,
    public_url: String,
) {
    if let Err(e) = connection_repo
        .update_status(conn.id, SyncStatus::Syncing, None)
        .await
    {
        tracing::error!(conn_id = %conn.id, "failed to set sync status to Syncing: {e}");
        return;
    }
    tracing::info!(conn_id = %conn.id, "starting monobank sync");
    // let webhook_url = format!("{public_url}/monobank/webhook");
    // if let Err(e) = monobank_client.set_webhook(&conn.token, &webhook_url).await {
    //     tracing::warn!(conn_id = %conn.id, "failed to set monobank webhook: {e}");
    // }

    let now = Utc::now();
    let mut cursor = history_from;

    while cursor < now {
        let to = (cursor + chrono::Duration::days(31)).min(now);

        let items = match monobank_client
            .get_statement(&conn.token, &conn.monobank_account_id, cursor, to)
            .await
        {
            Ok(items) => items,
            Err(e) => {
                tracing::error!(conn_id = %conn.id, "failed to fetch monobank statement: {e}");
                if let Err(e2) = connection_repo
                    .update_status(conn.id, SyncStatus::Failed, None)
                    .await
                {
                    tracing::error!(conn_id = %conn.id, "failed to set sync status to Failed: {e2}");
                }
                return;
            }
        };
        tracing::info!("in the loop {}", items.len());
        for item in &items {
            let tx = build_transaction(conn.account_id, conn.user_id, item);

            if let Err(e) = transaction_repo.create_idempotent(&tx).await {
                tracing::error!(
                    conn_id = %conn.id,
                    item_id = %item.id,
                    "failed to insert statement item: {e}"
                );
            }
        }

        cursor = to;
        if cursor < now {
            tokio::time::sleep(tokio::time::Duration::from_secs(61)).await;
        }
    }
    tracing::info!(conn_id = %conn.id, "monobank sync completed");
    if let Err(e) = connection_repo
        .update_status(conn.id, SyncStatus::Completed, Some(Utc::now()))
        .await
    {
        tracing::error!(conn_id = %conn.id, "failed to set sync status to Completed: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::transaction::{TransactionDetails, TransactionListParams};
    use std::sync::Mutex;

    // --- Mock implementations ---

    struct MockConnectionRepo {
        connections: Mutex<Vec<MonobankConnection>>,
    }

    impl MockConnectionRepo {
        fn new() -> Self {
            Self {
                connections: Mutex::new(vec![]),
            }
        }

        fn with(connections: Vec<MonobankConnection>) -> Self {
            Self {
                connections: Mutex::new(connections),
            }
        }
    }

    #[async_trait::async_trait]
    impl MonobankConnectionRepository for MockConnectionRepo {
        async fn create(&self, conn: &MonobankConnection) -> anyhow::Result<()> {
            self.connections.lock().unwrap().push(conn.clone());
            Ok(())
        }

        async fn find_by_id(
            &self,
            id: Uuid,
            user_id: Uuid,
        ) -> anyhow::Result<Option<MonobankConnection>> {
            Ok(self
                .connections
                .lock()
                .unwrap()
                .iter()
                .find(|c| c.id == id && c.user_id == user_id)
                .cloned())
        }

        async fn find_by_monobank_account_id(
            &self,
            monobank_account_id: &str,
        ) -> anyhow::Result<Option<MonobankConnection>> {
            Ok(self
                .connections
                .lock()
                .unwrap()
                .iter()
                .find(|c| c.monobank_account_id == monobank_account_id)
                .cloned())
        }

        async fn list_by_user(&self, user_id: Uuid) -> anyhow::Result<Vec<MonobankConnection>> {
            Ok(self
                .connections
                .lock()
                .unwrap()
                .iter()
                .filter(|c| c.user_id == user_id)
                .cloned()
                .collect())
        }

        async fn list_incomplete(&self) -> anyhow::Result<Vec<MonobankConnection>> {
            Ok(self
                .connections
                .lock()
                .unwrap()
                .iter()
                .filter(|c| matches!(c.sync_status, SyncStatus::Pending | SyncStatus::Syncing))
                .cloned()
                .collect())
        }

        async fn update_status(
            &self,
            id: Uuid,
            status: SyncStatus,
            last_synced_at: Option<DateTime<Utc>>,
        ) -> anyhow::Result<()> {
            let mut conns = self.connections.lock().unwrap();
            if let Some(c) = conns.iter_mut().find(|c| c.id == id) {
                c.sync_status = status;
                c.last_synced_at = last_synced_at;
            }
            Ok(())
        }

        async fn delete(&self, id: Uuid, user_id: Uuid) -> anyhow::Result<()> {
            let mut conns = self.connections.lock().unwrap();
            conns.retain(|c| !(c.id == id && c.user_id == user_id));
            Ok(())
        }
    }

    struct MockTransactionRepo {
        transactions: Mutex<Vec<Transaction>>,
    }

    impl MockTransactionRepo {
        fn new() -> Self {
            Self {
                transactions: Mutex::new(vec![]),
            }
        }
    }

    #[async_trait::async_trait]
    impl TransactionRepository for MockTransactionRepo {
        async fn create(
            &self,
            tx: &Transaction,
            _details: &TransactionDetails,
        ) -> anyhow::Result<()> {
            self.transactions.lock().unwrap().push(tx.clone());
            Ok(())
        }

        async fn find_by_id(
            &self,
            id: Uuid,
            user_id: Uuid,
        ) -> anyhow::Result<Option<(Transaction, TransactionDetails)>> {
            Ok(self
                .transactions
                .lock()
                .unwrap()
                .iter()
                .find(|t| t.id == id && t.user_id == user_id)
                .map(|t| (t.clone(), TransactionDetails::None)))
        }

        async fn list(
            &self,
            _params: &TransactionListParams,
        ) -> anyhow::Result<Vec<(Transaction, TransactionDetails)>> {
            Ok(self
                .transactions
                .lock()
                .unwrap()
                .iter()
                .map(|t| (t.clone(), TransactionDetails::None))
                .collect())
        }

        async fn update(
            &self,
            _tx: &Transaction,
            _details: &TransactionDetails,
        ) -> anyhow::Result<()> {
            Ok(())
        }

        async fn delete(&self, id: Uuid, user_id: Uuid) -> anyhow::Result<()> {
            let mut txs = self.transactions.lock().unwrap();
            txs.retain(|t| !(t.id == id && t.user_id == user_id));
            Ok(())
        }

        async fn create_idempotent(&self, tx: &Transaction) -> anyhow::Result<()> {
            let mut txs = self.transactions.lock().unwrap();
            let already_exists = tx.external_id.as_ref().is_some_and(|eid| {
                txs.iter()
                    .any(|t| t.external_id.as_deref() == Some(eid.as_str()))
            });
            if !already_exists {
                txs.push(tx.clone());
            }
            Ok(())
        }
    }

    struct MockMonobankClient;

    #[async_trait::async_trait]
    impl MonobankApiClient for MockMonobankClient {
        async fn get_accounts(&self, _token: &str) -> anyhow::Result<Vec<MonoAccount>> {
            Ok(vec![])
        }

        async fn get_statement(
            &self,
            _token: &str,
            _account_id: &str,
            _from: DateTime<Utc>,
            _to: DateTime<Utc>,
        ) -> anyhow::Result<Vec<MonoStatementItem>> {
            Ok(vec![])
        }

        async fn set_webhook(&self, _token: &str, _webhook_url: &str) -> anyhow::Result<()> {
            Ok(())
        }
    }

    fn make_service(
        conn_repo: Arc<dyn MonobankConnectionRepository>,
        tx_repo: Arc<dyn TransactionRepository>,
    ) -> MonobankService {
        MonobankService::new(
            conn_repo,
            tx_repo,
            Arc::new(MockMonobankClient),
            "https://example.com".to_string(),
        )
    }

    fn make_statement_item(id: &str, amount: i64) -> MonoStatementItem {
        MonoStatementItem {
            id: id.to_string(),
            time: 1_700_000_000,
            description: Some("Test payment".to_string()),
            mcc: 5411,
            amount,
            operation_amount: amount,
            currency_code: 980,
            balance: 1_000_000,
            hold: false,
        }
    }

    // --- Tests ---

    #[tokio::test]
    async fn connect_saves_connection_as_pending() {
        let conn_repo: Arc<dyn MonobankConnectionRepository> = Arc::new(MockConnectionRepo::new());
        let tx_repo: Arc<dyn TransactionRepository> = Arc::new(MockTransactionRepo::new());
        let svc = make_service(Arc::clone(&conn_repo), Arc::clone(&tx_repo));

        let account_id = Uuid::new_v4();
        let user_id = Uuid::new_v4();

        let conn = svc
            .connect(
                account_id,
                user_id,
                "test-token".to_string(),
                "mono-acc-123".to_string(),
                Utc::now(),
            )
            .await
            .expect("connect should succeed");

        assert_eq!(conn.account_id, account_id);
        assert_eq!(conn.user_id, user_id);
        assert_eq!(conn.sync_status, SyncStatus::Pending);

        let stored = conn_repo
            .find_by_id(conn.id, user_id)
            .await
            .unwrap()
            .expect("connection should be stored");
        assert_eq!(stored.sync_status, SyncStatus::Pending);
    }

    #[tokio::test]
    async fn handle_webhook_inserts_income_transaction() {
        let account_id = Uuid::new_v4();
        let user_id = Uuid::new_v4();

        let existing_conn = MonobankConnection::new(
            account_id,
            user_id,
            "tok".to_string(),
            "mono-acc-456".to_string(),
        );

        let conn_repo: Arc<dyn MonobankConnectionRepository> =
            Arc::new(MockConnectionRepo::with(vec![existing_conn]));
        let tx_repo: Arc<dyn TransactionRepository> = Arc::new(MockTransactionRepo::new());
        let svc = make_service(Arc::clone(&conn_repo), Arc::clone(&tx_repo));

        let item = make_statement_item("ext-id-001", 5000); // positive = income

        let result = svc
            .handle_webhook("mono-acc-456", &item)
            .await
            .expect("handle_webhook should succeed");

        assert_eq!(result, 1);

        let tx_repo_inner = tx_repo
            .list(&TransactionListParams {
                account_id: Some(account_id),
                user_id,
                kind: None,
                category_id: None,
                from: None,
                to: None,
                limit: 100,
                offset: 0,
            })
            .await
            .unwrap();

        assert_eq!(tx_repo_inner.len(), 1);
        let (tx, _) = &tx_repo_inner[0];
        assert_eq!(tx.external_id.as_deref(), Some("ext-id-001"));
        assert_eq!(tx.kind, TransactionKind::Income);
        assert_eq!(tx.account_id, account_id);
    }

    #[tokio::test]
    async fn handle_webhook_unknown_account_returns_zero() {
        let conn_repo: Arc<dyn MonobankConnectionRepository> = Arc::new(MockConnectionRepo::new());
        let tx_repo: Arc<dyn TransactionRepository> = Arc::new(MockTransactionRepo::new());
        let svc = make_service(Arc::clone(&conn_repo), Arc::clone(&tx_repo));

        let item = make_statement_item("ext-id-999", -1000);

        let result = svc
            .handle_webhook("unknown-account-id", &item)
            .await
            .expect("handle_webhook should return Ok even for unknown account");

        assert_eq!(result, 0);
    }
}
