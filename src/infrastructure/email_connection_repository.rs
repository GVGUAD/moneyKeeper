use std::sync::Arc;

use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use crate::domain::email_connection::{
    EmailConnection, EmailConnectionRepository, EmailConnectionStatus, EmailProvider,
};
use crate::infrastructure::credential_crypto::{
    SecretValue, TokenCipher, email_access_token_aad, email_refresh_token_aad,
};

pub struct PgEmailConnectionRepository {
    pool: PgPool,
    cipher: Option<Arc<TokenCipher>>,
}

impl PgEmailConnectionRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool, cipher: None }
    }

    pub fn with_cipher(pool: PgPool, cipher: Arc<TokenCipher>) -> Self {
        Self {
            pool,
            cipher: Some(cipher),
        }
    }

    fn credentials_for_write(
        &self,
        id: Uuid,
        access_token: &str,
        refresh_token: &str,
    ) -> anyhow::Result<(SecretValue, SecretValue, Option<String>, Option<String>)> {
        let Some(cipher) = &self.cipher else {
            return Ok((access_token.into(), refresh_token.into(), None, None));
        };
        let access_aad = email_access_token_aad(id);
        let refresh_aad = email_refresh_token_aad(id);
        Ok((
            cipher.legacy_write_value(access_token),
            cipher.legacy_write_value(refresh_token),
            Some(cipher.encrypt(access_token, access_aad.as_bytes())?),
            Some(cipher.encrypt(refresh_token, refresh_aad.as_bytes())?),
        ))
    }

    fn row_to_conn(&self, r: Row) -> anyhow::Result<EmailConnection> {
        let access_token: SecretValue = match r.oauth_access_token_encrypted {
            Some(encrypted) => {
                let cipher = self.cipher.as_ref().ok_or_else(|| {
                    anyhow::anyhow!("encrypted email credential requires TokenCipher")
                })?;
                let aad = email_access_token_aad(r.id);
                cipher.decrypt(&encrypted, aad.as_bytes())?
            }
            None => r.oauth_access_token.into(),
        };
        let refresh_token: SecretValue = match r.oauth_refresh_token_encrypted {
            Some(encrypted) => {
                let cipher = self.cipher.as_ref().ok_or_else(|| {
                    anyhow::anyhow!("encrypted email credential requires TokenCipher")
                })?;
                let aad = email_refresh_token_aad(r.id);
                cipher.decrypt(&encrypted, aad.as_bytes())?
            }
            None => r.oauth_refresh_token.into(),
        };
        Ok(EmailConnection {
            id: r.id,
            user_id: r.user_id,
            provider: EmailProvider::from_str(&r.provider)?,
            email_address: r.email_address,
            oauth_access_token: access_token,
            oauth_refresh_token: refresh_token,
            credential_version: r.credential_version,
            access_token_expires_at: DateTime::from_timestamp(r.access_token_expires_at, 0)
                .ok_or_else(|| anyhow::anyhow!("invalid access_token_expires_at"))?,
            status: EmailConnectionStatus::from_str(&r.status)?,
            last_synced_at: r
                .last_synced_at
                .and_then(|t| DateTime::from_timestamp(t, 0)),
            last_history_id: r.last_history_id,
            created_at: DateTime::from_timestamp(r.created_at, 0)
                .ok_or_else(|| anyhow::anyhow!("invalid created_at"))?,
        })
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
    oauth_access_token_encrypted: Option<String>,
    oauth_refresh_token_encrypted: Option<String>,
    credential_version: i64,
    access_token_expires_at: i64,
    status: String,
    last_synced_at: Option<i64>,
    last_history_id: Option<String>,
    created_at: i64,
}

#[async_trait::async_trait]
impl EmailConnectionRepository for PgEmailConnectionRepository {
    async fn create(&self, conn: &EmailConnection) -> anyhow::Result<()> {
        let (access, refresh, access_encrypted, refresh_encrypted) = self.credentials_for_write(
            conn.id,
            &conn.oauth_access_token,
            &conn.oauth_refresh_token,
        )?;
        sqlx::query(
            "INSERT INTO email_connections \
             (id, user_id, provider, email_address, oauth_access_token, oauth_refresh_token, \
              oauth_access_token_encrypted, oauth_refresh_token_encrypted, access_token_expires_at, \
              status, last_synced_at, last_history_id, created_at) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13)",
        )
        .bind(conn.id)
        .bind(conn.user_id)
        .bind(conn.provider.as_str())
        .bind(conn.email_address.trim().to_ascii_lowercase())
        .bind(access.expose())
        .bind(refresh.expose())
        .bind(access_encrypted)
        .bind(refresh_encrypted)
        .bind(conn.access_token_expires_at.timestamp())
        .bind(conn.status.as_str())
        .bind(conn.last_synced_at.map(|d| d.timestamp()))
        .bind(&conn.last_history_id)
        .bind(conn.created_at.timestamp())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn upsert_by_address(&self, conn: &EmailConnection) -> anyhow::Result<EmailConnection> {
        let normalized_address = conn.email_address.trim().to_ascii_lowercase();
        let mut transaction = self.pool.begin().await?;
        sqlx::query(
            "SELECT pg_advisory_xact_lock(hashtextextended(\
                $1::uuid::text || ':' || $2 || ':' || $3, 0\
             ))",
        )
        .bind(conn.user_id)
        .bind(conn.provider.as_str())
        .bind(&normalized_address)
        .execute(&mut *transaction)
        .await?;
        // Ciphertext AAD includes the entity id. Reconnects must therefore
        // encrypt against the existing row id, not the throw-away id supplied
        // by the OAuth completion attempt.
        let effective_id = sqlx::query_scalar::<_, Uuid>(
            "SELECT id FROM email_connections \
             WHERE user_id=$1 AND provider=$2 AND lower(btrim(email_address))=$3 FOR UPDATE",
        )
        .bind(conn.user_id)
        .bind(conn.provider.as_str())
        .bind(&normalized_address)
        .fetch_optional(&mut *transaction)
        .await?
        .unwrap_or(conn.id);
        let (access, refresh, access_encrypted, refresh_encrypted) = self.credentials_for_write(
            effective_id,
            &conn.oauth_access_token,
            &conn.oauth_refresh_token,
        )?;
        let row = sqlx::query_as::<_, Row>(
            "INSERT INTO email_connections \
             (id, user_id, provider, email_address, oauth_access_token, oauth_refresh_token, \
              oauth_access_token_encrypted, oauth_refresh_token_encrypted, access_token_expires_at, \
              status, last_synced_at, last_history_id, created_at) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13) \
             ON CONFLICT (user_id, provider, (lower(btrim(email_address)))) DO UPDATE SET \
               email_address=EXCLUDED.email_address, \
               oauth_access_token=EXCLUDED.oauth_access_token, \
               oauth_refresh_token=EXCLUDED.oauth_refresh_token, \
               oauth_access_token_encrypted=EXCLUDED.oauth_access_token_encrypted, \
               oauth_refresh_token_encrypted=EXCLUDED.oauth_refresh_token_encrypted, \
               access_token_expires_at=EXCLUDED.access_token_expires_at, \
               credential_version=email_connections.credential_version + 1, \
               status='connected', next_sync_at=0, sync_attempts=0, \
               sync_last_error_kind=NULL \
             RETURNING *",
        )
        .bind(effective_id)
        .bind(conn.user_id)
        .bind(conn.provider.as_str())
        .bind(normalized_address)
        .bind(access.expose())
        .bind(refresh.expose())
        .bind(access_encrypted)
        .bind(refresh_encrypted)
        .bind(conn.access_token_expires_at.timestamp())
        .bind(conn.status.as_str())
        .bind(conn.last_synced_at.map(|d| d.timestamp()))
        .bind(&conn.last_history_id)
        .bind(conn.created_at.timestamp())
        .fetch_one(&mut *transaction)
        .await?;
        let connection = self.row_to_conn(row)?;
        transaction.commit().await?;
        Ok(connection)
    }

    async fn find_by_id(&self, id: Uuid, user_id: Uuid) -> anyhow::Result<Option<EmailConnection>> {
        let row = sqlx::query_as::<_, Row>(
            "SELECT * FROM email_connections \
                 WHERE id=$1 AND user_id=$2 AND status <> 'disconnected'",
        )
        .bind(id)
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|value| self.row_to_conn(value)).transpose()
    }

    async fn list_by_user(&self, user_id: Uuid) -> anyhow::Result<Vec<EmailConnection>> {
        let rows = sqlx::query_as::<_, Row>(
            "SELECT * FROM email_connections \
             WHERE user_id=$1 AND status <> 'disconnected' ORDER BY created_at",
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|value| self.row_to_conn(value))
            .collect()
    }

    async fn list_connected(&self) -> anyhow::Result<Vec<EmailConnection>> {
        let rows = sqlx::query_as::<_, Row>(
            "SELECT * FROM email_connections WHERE status='connected' ORDER BY created_at",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|value| self.row_to_conn(value))
            .collect()
    }

    async fn update_tokens(
        &self,
        id: Uuid,
        expected_credential_version: i64,
        access_token: &str,
        refresh_token: &str,
        expires_at: DateTime<Utc>,
    ) -> anyhow::Result<bool> {
        let (access, refresh, access_encrypted, refresh_encrypted) =
            self.credentials_for_write(id, access_token, refresh_token)?;
        let result = sqlx::query(
            "UPDATE email_connections SET oauth_access_token=$1, oauth_refresh_token=$2, \
             oauth_access_token_encrypted=$3, oauth_refresh_token_encrypted=$4, \
             access_token_expires_at=$5, credential_version=credential_version+1 \
             WHERE id=$6 AND credential_version=$7 AND status='connected'",
        )
        .bind(access.expose())
        .bind(refresh.expose())
        .bind(access_encrypted)
        .bind(refresh_encrypted)
        .bind(expires_at.timestamp())
        .bind(id)
        .bind(expected_credential_version)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() == 1)
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
        // Preserve the normalized mailbox identity and ingestion ledger so a
        // later reconnect reuses the same idempotency namespace. OAuth has
        // already revoked the token before this transition.
        sqlx::query(
            "UPDATE email_connections SET status='disconnected', next_sync_at=0, \
               sync_lease_owner=NULL, sync_lease_expires_at=NULL \
             WHERE id=$1 AND user_id=$2",
        )
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
    use crate::infrastructure::credential_crypto::TokenCipherConfig;
    use crate::infrastructure::test_db;

    fn test_cipher() -> Arc<TokenCipher> {
        Arc::new(TokenCipher::new(TokenCipherConfig {
            active_key_id: "test".to_string(),
            keys: [("test".to_string(), [17_u8; 32])].into_iter().collect(),
        }))
    }

    fn sample_conn(user_id: Uuid) -> EmailConnection {
        EmailConnection {
            id: Uuid::new_v4(),
            user_id,
            provider: EmailProvider::Gmail,
            email_address: "alice@example.com".to_string(),
            oauth_access_token: "access-1".into(),
            oauth_refresh_token: "refresh-1".into(),
            credential_version: 0,
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
        let mut conn = sample_conn(user_id);
        conn.status = EmailConnectionStatus::Connected;
        let id = conn.id;
        repo.create(&conn).await.unwrap();
        let new_exp = Utc::now() + chrono::Duration::hours(2);
        assert!(
            repo.update_tokens(id, 0, "new-access", "new-refresh", new_exp)
                .await
                .unwrap()
        );
        let found = repo.find_by_id(id, user_id).await.unwrap().unwrap();
        assert_eq!(found.oauth_access_token, "new-access");
        assert_eq!(found.oauth_refresh_token, "new-refresh");
        assert_eq!(found.credential_version, 1);
        assert_eq!(
            found.access_token_expires_at.timestamp(),
            new_exp.timestamp()
        );
    }

    #[tokio::test]
    async fn list_connected_filters_by_status() {
        let pool = test_db::fresh_pool().await;
        let repo = PgEmailConnectionRepository::new(pool);
        let user_id = Uuid::new_v4();
        let pending = sample_conn(user_id);
        let mut connected = sample_conn(user_id);
        connected.email_address = "connected@example.com".to_string();
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

    #[tokio::test]
    async fn encrypted_repository_dual_writes_and_prefers_ciphertext() {
        let pool = test_db::fresh_pool().await;
        let repo = PgEmailConnectionRepository::with_cipher(pool.clone(), test_cipher());
        let conn = sample_conn(Uuid::new_v4());
        repo.create(&conn).await.unwrap();

        let raw: (String, String, Option<String>, Option<String>) = sqlx::query_as(
            "SELECT oauth_access_token, oauth_refresh_token, \
                    oauth_access_token_encrypted, oauth_refresh_token_encrypted \
             FROM email_connections WHERE id=$1",
        )
        .bind(conn.id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(raw.0, "access-1", "plaintext retained during rollout");
        assert_eq!(raw.1, "refresh-1", "plaintext retained during rollout");
        assert!(raw.2.as_deref().unwrap().starts_with("enc:v1:test:"));
        assert!(raw.3.as_deref().unwrap().starts_with("enc:v1:test:"));

        let found = repo
            .find_by_id(conn.id, conn.user_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(found.oauth_access_token, "access-1");
        assert_eq!(found.oauth_refresh_token, "refresh-1");
        assert!(
            PgEmailConnectionRepository::new(pool)
                .find_by_id(conn.id, conn.user_id)
                .await
                .is_err(),
            "encrypted rows must not silently fall back to plaintext"
        );
    }

    #[tokio::test]
    async fn upsert_normalizes_address_and_preserves_connection_identity() {
        let pool = test_db::fresh_pool().await;
        let repo = PgEmailConnectionRepository::with_cipher(pool, test_cipher());
        let user_id = Uuid::new_v4();
        let mut first = sample_conn(user_id);
        first.email_address = " Alice@Example.COM ".to_string();
        let inserted = repo.upsert_by_address(&first).await.unwrap();

        let mut replacement = sample_conn(user_id);
        replacement.email_address = "alice@example.com".to_string();
        replacement.oauth_access_token = "access-2".into();
        replacement.oauth_refresh_token = "refresh-2".into();
        let updated = repo.upsert_by_address(&replacement).await.unwrap();

        assert_eq!(updated.id, inserted.id);
        assert_eq!(updated.email_address, "alice@example.com");
        assert_eq!(updated.oauth_access_token, "access-2");
        assert_eq!(repo.list_by_user(user_id).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn reconnect_preserves_sync_lease_and_fences_stale_token_refresh() {
        let pool = test_db::fresh_pool().await;
        let repo = PgEmailConnectionRepository::with_cipher(pool.clone(), test_cipher());
        let user_id = Uuid::new_v4();
        let mut first = sample_conn(user_id);
        first.status = EmailConnectionStatus::Connected;
        let inserted = repo.upsert_by_address(&first).await.unwrap();
        let lease_owner = Uuid::new_v4();
        sqlx::query(
            "UPDATE email_connections \
             SET sync_lease_owner=$1, sync_lease_expires_at=$2 \
             WHERE id=$3",
        )
        .bind(lease_owner)
        .bind((Utc::now() + chrono::Duration::minutes(10)).timestamp())
        .bind(inserted.id)
        .execute(&pool)
        .await
        .unwrap();

        let mut replacement = sample_conn(user_id);
        replacement.email_address = first.email_address.clone();
        replacement.oauth_access_token = "reconnected-access".into();
        replacement.oauth_refresh_token = "reconnected-refresh".into();
        replacement.status = EmailConnectionStatus::Connected;
        let updated = repo.upsert_by_address(&replacement).await.unwrap();

        assert_eq!(updated.id, inserted.id);
        assert_eq!(updated.credential_version, 1);
        let preserved_lease: (Option<Uuid>, Option<i64>) = sqlx::query_as(
            "SELECT sync_lease_owner, sync_lease_expires_at \
             FROM email_connections WHERE id=$1",
        )
        .bind(inserted.id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(preserved_lease.0, Some(lease_owner));
        assert!(preserved_lease.1.is_some());

        let stale_write = repo
            .update_tokens(
                inserted.id,
                0,
                "stale-access",
                "stale-refresh",
                Utc::now() + chrono::Duration::hours(2),
            )
            .await
            .unwrap();
        assert!(!stale_write);
        let found = repo
            .find_by_id(inserted.id, user_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(found.oauth_access_token, "reconnected-access");
        assert_eq!(found.oauth_refresh_token, "reconnected-refresh");
        assert_eq!(found.credential_version, 1);
    }
}
