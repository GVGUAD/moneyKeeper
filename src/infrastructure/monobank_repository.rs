use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use crate::domain::bank_connection::{BankConnection, BankConnectionRepository, BankProvider, SyncStatus};

pub struct PgBankConnectionRepository {
    pool: PgPool,
}

impl PgBankConnectionRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[derive(sqlx::FromRow)]
struct ConnectionRow {
    id: Uuid,
    account_id: Uuid,
    user_id: Uuid,
    provider: String,
    token: String,
    external_account_id: String,
    sync_status: String,
    last_synced_at: Option<i64>,
    created_at: i64,
}

fn row_to_conn(r: ConnectionRow) -> anyhow::Result<BankConnection> {
    Ok(BankConnection {
        id: r.id,
        account_id: r.account_id,
        user_id: r.user_id,
        provider: BankProvider::from_str(&r.provider)?,
        token: r.token,
        external_account_id: r.external_account_id,
        sync_status: SyncStatus::from_str(&r.sync_status)?,
        last_synced_at: r
            .last_synced_at
            .and_then(|ts| DateTime::from_timestamp(ts, 0)),
        created_at: DateTime::from_timestamp(r.created_at, 0)
            .ok_or_else(|| anyhow::anyhow!("invalid created_at timestamp"))?,
    })
}

#[async_trait::async_trait]
impl BankConnectionRepository for PgBankConnectionRepository {
    async fn create(&self, conn: &BankConnection) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO bank_connections \
             (id, account_id, user_id, provider, token, external_account_id, sync_status, \
              last_synced_at, created_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
        )
        .bind(conn.id)
        .bind(conn.account_id)
        .bind(conn.user_id)
        .bind(conn.provider.as_str())
        .bind(&conn.token)
        .bind(&conn.external_account_id)
        .bind(conn.sync_status.as_str())
        .bind(conn.last_synced_at.map(|dt| dt.timestamp()))
        .bind(conn.created_at.timestamp())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn find_by_id(
        &self,
        id: Uuid,
        user_id: Uuid,
    ) -> anyhow::Result<Option<BankConnection>> {
        let row = sqlx::query_as::<_, ConnectionRow>(
            "SELECT * FROM bank_connections WHERE id = $1 AND user_id = $2",
        )
        .bind(id)
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(row_to_conn).transpose()
    }

    async fn find_by_external_account_id(
        &self,
        provider: &BankProvider,
        external_account_id: &str,
    ) -> anyhow::Result<Option<BankConnection>> {
        let row = sqlx::query_as::<_, ConnectionRow>(
            "SELECT * FROM bank_connections WHERE provider = $1 AND external_account_id = $2",
        )
        .bind(provider.as_str())
        .bind(external_account_id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(row_to_conn).transpose()
    }

    async fn list_by_user(&self, user_id: Uuid) -> anyhow::Result<Vec<BankConnection>> {
        let rows = sqlx::query_as::<_, ConnectionRow>(
            "SELECT * FROM bank_connections WHERE user_id = $1 ORDER BY created_at",
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(row_to_conn).collect()
    }

    async fn list_incomplete(&self) -> anyhow::Result<Vec<BankConnection>> {
        let rows = sqlx::query_as::<_, ConnectionRow>(
            "SELECT * FROM bank_connections \
             WHERE sync_status IN ('pending', 'syncing') ORDER BY created_at",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(row_to_conn).collect()
    }

    async fn update_status(
        &self,
        id: Uuid,
        status: SyncStatus,
        last_synced_at: Option<DateTime<Utc>>,
    ) -> anyhow::Result<()> {
        sqlx::query(
            "UPDATE bank_connections \
             SET sync_status = $1, last_synced_at = $2 WHERE id = $3",
        )
        .bind(status.as_str())
        .bind(last_synced_at.map(|dt| dt.timestamp()))
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn delete(&self, id: Uuid, user_id: Uuid) -> anyhow::Result<()> {
        sqlx::query("DELETE FROM bank_connections WHERE id = $1 AND user_id = $2")
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
    use crate::domain::account::{Account, AccountDetails, AccountRepository, AccountType};
    use crate::infrastructure::account_repository::SqliteAccountRepository;
    use sqlx::PgPool;

    async fn make_account(pool: &PgPool) -> (Uuid, Uuid) {
        let user_id = Uuid::new_v4();
        let account = Account::new(
            user_id,
            "Monobank".to_string(),
            AccountType::Cash,
            "UAH".to_string(),
        );
        let account_id = account.id;
        SqliteAccountRepository::new(pool.clone())
            .create(&account, &AccountDetails::None)
            .await
            .unwrap();
        (user_id, account_id)
    }

    #[sqlx::test(migrations = "src/infrastructure/migrations")]
    async fn create_and_find_by_id(pool: PgPool) {
        let (user_id, account_id) = make_account(&pool).await;
        let repo = PgBankConnectionRepository::new(pool);
        let conn = BankConnection::new(
            account_id,
            user_id,
            BankProvider::Monobank,
            "test-token-123".to_string(),
            "mono-acc-abc".to_string(),
        );
        let conn_id = conn.id;
        repo.create(&conn).await.unwrap();
        let found = repo.find_by_id(conn_id, user_id).await.unwrap().unwrap();
        assert_eq!(found.token, "test-token-123");
        assert_eq!(found.sync_status, SyncStatus::Pending);
        assert_eq!(found.provider, BankProvider::Monobank);
    }

    #[sqlx::test(migrations = "src/infrastructure/migrations")]
    async fn list_incomplete_returns_pending_and_syncing(pool: PgPool) {
        let (user_id, account_id) = make_account(&pool).await;
        let repo = PgBankConnectionRepository::new(pool);

        let pending = BankConnection::new(
            account_id,
            user_id,
            BankProvider::Monobank,
            "token-pending".to_string(),
            "mono-pending".to_string(),
        );
        let syncing = BankConnection::new(
            account_id,
            user_id,
            BankProvider::Monobank,
            "token-syncing".to_string(),
            "mono-syncing".to_string(),
        );
        let completed = BankConnection::new(
            account_id,
            user_id,
            BankProvider::Monobank,
            "token-completed".to_string(),
            "mono-completed".to_string(),
        );

        repo.create(&pending).await.unwrap();
        repo.create(&syncing).await.unwrap();
        repo.update_status(syncing.id, SyncStatus::Syncing, None)
            .await
            .unwrap();
        repo.create(&completed).await.unwrap();
        repo.update_status(completed.id, SyncStatus::Completed, Some(Utc::now()))
            .await
            .unwrap();

        let incomplete = repo.list_incomplete().await.unwrap();
        assert_eq!(incomplete.len(), 2);
        let statuses: Vec<&SyncStatus> = incomplete.iter().map(|c| &c.sync_status).collect();
        assert!(statuses.contains(&&SyncStatus::Pending));
        assert!(statuses.contains(&&SyncStatus::Syncing));
    }

    #[sqlx::test(migrations = "src/infrastructure/migrations")]
    async fn update_status_changes_sync_status(pool: PgPool) {
        let (user_id, account_id) = make_account(&pool).await;
        let repo = PgBankConnectionRepository::new(pool);
        let conn = BankConnection::new(
            account_id,
            user_id,
            BankProvider::Monobank,
            "token-update".to_string(),
            "mono-update".to_string(),
        );
        let conn_id = conn.id;
        repo.create(&conn).await.unwrap();
        repo.update_status(conn_id, SyncStatus::Completed, Some(Utc::now()))
            .await
            .unwrap();
        let found = repo.find_by_id(conn_id, user_id).await.unwrap().unwrap();
        assert_eq!(found.sync_status, SyncStatus::Completed);
        assert!(found.last_synced_at.is_some());
    }
}
