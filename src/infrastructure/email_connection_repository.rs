use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use crate::domain::email_connection::{
    EmailConnection, EmailConnectionRepository, EmailConnectionStatus, EmailProvider,
};

pub struct PgEmailConnectionRepository {
    pool: PgPool,
}

impl PgEmailConnectionRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[derive(sqlx::FromRow)]
struct Row {
    id: Uuid,
    user_id: Uuid,
    provider: String,
    email_address: String,
    oauth_access_token: String,
    oauth_refresh_token: String,
    access_token_expires_at: i64,
    status: String,
    last_synced_at: Option<i64>,
    last_history_id: Option<String>,
    created_at: i64,
}

fn row_to_conn(r: Row) -> anyhow::Result<EmailConnection> {
    Ok(EmailConnection {
        id: r.id,
        user_id: r.user_id,
        provider: EmailProvider::from_str(&r.provider)?,
        email_address: r.email_address,
        oauth_access_token: r.oauth_access_token,
        oauth_refresh_token: r.oauth_refresh_token,
        access_token_expires_at: DateTime::from_timestamp(r.access_token_expires_at, 0)
            .ok_or_else(|| anyhow::anyhow!("invalid access_token_expires_at"))?,
        status: EmailConnectionStatus::from_str(&r.status)?,
        last_synced_at: r.last_synced_at.and_then(|t| DateTime::from_timestamp(t, 0)),
        last_history_id: r.last_history_id,
        created_at: DateTime::from_timestamp(r.created_at, 0)
            .ok_or_else(|| anyhow::anyhow!("invalid created_at"))?,
    })
}

#[async_trait::async_trait]
impl EmailConnectionRepository for PgEmailConnectionRepository {
    async fn create(&self, conn: &EmailConnection) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO email_connections \
             (id, user_id, provider, email_address, oauth_access_token, oauth_refresh_token, \
              access_token_expires_at, status, last_synced_at, last_history_id, created_at) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)",
        )
        .bind(conn.id)
        .bind(conn.user_id)
        .bind(conn.provider.as_str())
        .bind(&conn.email_address)
        .bind(&conn.oauth_access_token)
        .bind(&conn.oauth_refresh_token)
        .bind(conn.access_token_expires_at.timestamp())
        .bind(conn.status.as_str())
        .bind(conn.last_synced_at.map(|d| d.timestamp()))
        .bind(&conn.last_history_id)
        .bind(conn.created_at.timestamp())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn find_by_id(&self, id: Uuid, user_id: Uuid) -> anyhow::Result<Option<EmailConnection>> {
        let row = sqlx::query_as::<_, Row>(
            "SELECT * FROM email_connections WHERE id=$1 AND user_id=$2",
        )
        .bind(id)
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(row_to_conn).transpose()
    }

    async fn list_by_user(&self, user_id: Uuid) -> anyhow::Result<Vec<EmailConnection>> {
        let rows = sqlx::query_as::<_, Row>(
            "SELECT * FROM email_connections WHERE user_id=$1 ORDER BY created_at",
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(row_to_conn).collect()
    }

    async fn list_connected(&self) -> anyhow::Result<Vec<EmailConnection>> {
        let rows = sqlx::query_as::<_, Row>(
            "SELECT * FROM email_connections WHERE status='connected' ORDER BY created_at",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(row_to_conn).collect()
    }

    async fn update_tokens(
        &self,
        id: Uuid,
        access_token: &str,
        refresh_token: &str,
        expires_at: DateTime<Utc>,
    ) -> anyhow::Result<()> {
        sqlx::query(
            "UPDATE email_connections SET oauth_access_token=$1, oauth_refresh_token=$2, \
             access_token_expires_at=$3 WHERE id=$4",
        )
        .bind(access_token)
        .bind(refresh_token)
        .bind(expires_at.timestamp())
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn update_status(&self, id: Uuid, status: EmailConnectionStatus) -> anyhow::Result<()> {
        sqlx::query("UPDATE email_connections SET status=$1 WHERE id=$2")
            .bind(status.as_str())
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn update_sync_cursor(
        &self,
        id: Uuid,
        last_synced_at: DateTime<Utc>,
        last_history_id: Option<String>,
    ) -> anyhow::Result<()> {
        sqlx::query(
            "UPDATE email_connections SET last_synced_at=$1, last_history_id=$2 WHERE id=$3",
        )
        .bind(last_synced_at.timestamp())
        .bind(last_history_id)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn delete(&self, id: Uuid, user_id: Uuid) -> anyhow::Result<()> {
        sqlx::query("DELETE FROM email_connections WHERE id=$1 AND user_id=$2")
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
    use crate::infrastructure::test_db;

    fn sample_conn(user_id: Uuid) -> EmailConnection {
        EmailConnection {
            id: Uuid::new_v4(),
            user_id,
            provider: EmailProvider::Gmail,
            email_address: "alice@example.com".to_string(),
            oauth_access_token: "access-1".to_string(),
            oauth_refresh_token: "refresh-1".to_string(),
            access_token_expires_at: Utc::now() + chrono::Duration::hours(1),
            status: EmailConnectionStatus::Pending,
            last_synced_at: None,
            last_history_id: None,
            created_at: Utc::now(),
        }
    }

    #[tokio::test]
    async fn create_and_find_by_id() {
        let pool = test_db::fresh_pool().await;
        let repo = PgEmailConnectionRepository::new(pool);
        let user_id = Uuid::new_v4();
        let conn = sample_conn(user_id);
        let id = conn.id;
        repo.create(&conn).await.unwrap();
        let found = repo.find_by_id(id, user_id).await.unwrap().unwrap();
        assert_eq!(found.email_address, "alice@example.com");
        assert_eq!(found.status, EmailConnectionStatus::Pending);
    }

    #[tokio::test]
    async fn update_tokens_persists() {
        let pool = test_db::fresh_pool().await;
        let repo = PgEmailConnectionRepository::new(pool);
        let user_id = Uuid::new_v4();
        let conn = sample_conn(user_id);
        let id = conn.id;
        repo.create(&conn).await.unwrap();
        let new_exp = Utc::now() + chrono::Duration::hours(2);
        repo.update_tokens(id, "new-access", "new-refresh", new_exp)
            .await
            .unwrap();
        let found = repo.find_by_id(id, user_id).await.unwrap().unwrap();
        assert_eq!(found.oauth_access_token, "new-access");
        assert_eq!(found.oauth_refresh_token, "new-refresh");
        assert_eq!(found.access_token_expires_at.timestamp(), new_exp.timestamp());
    }

    #[tokio::test]
    async fn list_connected_filters_by_status() {
        let pool = test_db::fresh_pool().await;
        let repo = PgEmailConnectionRepository::new(pool);
        let user_id = Uuid::new_v4();
        let pending = sample_conn(user_id);
        let connected = sample_conn(user_id);
        repo.create(&pending).await.unwrap();
        repo.create(&connected).await.unwrap();
        repo.update_status(connected.id, EmailConnectionStatus::Connected)
            .await
            .unwrap();
        let all = repo.list_connected().await.unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].id, connected.id);
    }

    #[tokio::test]
    async fn update_sync_cursor_persists() {
        let pool = test_db::fresh_pool().await;
        let repo = PgEmailConnectionRepository::new(pool);
        let user_id = Uuid::new_v4();
        let conn = sample_conn(user_id);
        let id = conn.id;
        repo.create(&conn).await.unwrap();
        let now = Utc::now();
        repo.update_sync_cursor(id, now, Some("hist-42".to_string()))
            .await
            .unwrap();
        let found = repo.find_by_id(id, user_id).await.unwrap().unwrap();
        assert_eq!(found.last_history_id.as_deref(), Some("hist-42"));
        assert_eq!(found.last_synced_at.unwrap().timestamp(), now.timestamp());
    }
}
