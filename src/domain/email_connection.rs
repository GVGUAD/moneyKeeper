use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::domain::secret::SecretString;

#[derive(Debug, Clone, PartialEq)]
pub enum EmailProvider {
    Gmail,
}

impl EmailProvider {
    pub fn as_str(&self) -> &'static str {
        match self {
            EmailProvider::Gmail => "gmail",
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> anyhow::Result<Self> {
        match s {
            "gmail" => Ok(EmailProvider::Gmail),
            other => Err(anyhow::anyhow!("unknown email provider: {other}")),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum EmailConnectionStatus {
    Pending,
    Connected,
    Failed,
    ReconnectRequired,
    Disconnected,
}

impl EmailConnectionStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Connected => "connected",
            Self::Failed => "failed",
            Self::ReconnectRequired => "reconnect_required",
            Self::Disconnected => "disconnected",
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> anyhow::Result<Self> {
        match s {
            "pending" => Ok(Self::Pending),
            "connected" => Ok(Self::Connected),
            "failed" => Ok(Self::Failed),
            "reconnect_required" => Ok(Self::ReconnectRequired),
            "disconnected" => Ok(Self::Disconnected),
            other => Err(anyhow::anyhow!("unknown email connection status: {other}")),
        }
    }
}

#[derive(Debug, Clone)]
pub struct EmailConnection {
    pub id: Uuid,
    pub user_id: Uuid,
    pub provider: EmailProvider,
    pub email_address: String,
    pub oauth_access_token: SecretString,
    pub oauth_refresh_token: SecretString,
    pub credential_version: i64,
    pub access_token_expires_at: DateTime<Utc>,
    pub status: EmailConnectionStatus,
    pub last_synced_at: Option<DateTime<Utc>>,
    pub last_history_id: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[async_trait::async_trait]
pub trait EmailConnectionRepository: Send + Sync {
    async fn create(&self, conn: &EmailConnection) -> anyhow::Result<()>;
    /// Creates a connection or replaces credentials for the same normalized
    /// user/provider/email identity while preserving its id and sync cursor.
    async fn upsert_by_address(&self, conn: &EmailConnection) -> anyhow::Result<EmailConnection>;
    async fn find_by_id(&self, id: Uuid, user_id: Uuid) -> anyhow::Result<Option<EmailConnection>>;
    async fn list_by_user(&self, user_id: Uuid) -> anyhow::Result<Vec<EmailConnection>>;
    async fn list_connected(&self) -> anyhow::Result<Vec<EmailConnection>>;
    async fn update_tokens(
        &self,
        id: Uuid,
        expected_credential_version: i64,
        access_token: &str,
        refresh_token: &str,
        expires_at: DateTime<Utc>,
    ) -> anyhow::Result<bool>;
    async fn update_status(&self, id: Uuid, status: EmailConnectionStatus) -> anyhow::Result<()>;
    async fn update_sync_cursor(
        &self,
        id: Uuid,
        last_synced_at: DateTime<Utc>,
        last_history_id: Option<String>,
    ) -> anyhow::Result<()>;
    async fn delete(&self, id: Uuid, user_id: Uuid) -> anyhow::Result<()>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn email_provider_roundtrip() {
        let p = EmailProvider::Gmail;
        assert_eq!(EmailProvider::from_str(p.as_str()).unwrap(), p);
    }

    #[test]
    fn status_roundtrip() {
        for s in [
            EmailConnectionStatus::Pending,
            EmailConnectionStatus::Connected,
            EmailConnectionStatus::Failed,
            EmailConnectionStatus::ReconnectRequired,
            EmailConnectionStatus::Disconnected,
        ] {
            assert_eq!(EmailConnectionStatus::from_str(s.as_str()).unwrap(), s);
        }
    }

    #[test]
    fn unknown_provider_errors() {
        assert!(EmailProvider::from_str("yahoo").is_err());
    }
}
