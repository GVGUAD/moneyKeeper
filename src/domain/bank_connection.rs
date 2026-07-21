use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::domain::secret::SecretString;

#[derive(Debug, Clone, PartialEq)]
pub enum BankProvider {
    Monobank,
}

impl BankProvider {
    pub fn as_str(&self) -> &'static str {
        match self {
            BankProvider::Monobank => "monobank",
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> anyhow::Result<Self> {
        match s {
            "monobank" => Ok(BankProvider::Monobank),
            other => Err(anyhow::anyhow!("unknown bank provider: {other}")),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum SyncStatus {
    Pending,
    Syncing,
    Completed,
    Failed,
}

impl SyncStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            SyncStatus::Pending => "pending",
            SyncStatus::Syncing => "syncing",
            SyncStatus::Completed => "completed",
            SyncStatus::Failed => "failed",
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> anyhow::Result<Self> {
        match s {
            "pending" => Ok(SyncStatus::Pending),
            "syncing" => Ok(SyncStatus::Syncing),
            "completed" => Ok(SyncStatus::Completed),
            "failed" => Ok(SyncStatus::Failed),
            other => Err(anyhow::anyhow!("unknown sync_status: {other}")),
        }
    }
}

#[derive(Debug, Clone)]
pub struct BankConnection {
    pub id: Uuid,
    pub account_id: Uuid,
    pub user_id: Uuid,
    pub provider: BankProvider,
    pub token: SecretString,
    pub external_account_id: String,
    pub sync_status: SyncStatus,
    pub last_synced_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

impl BankConnection {
    pub fn new(
        account_id: Uuid,
        user_id: Uuid,
        provider: BankProvider,
        token: impl Into<SecretString>,
        external_account_id: String,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            account_id,
            user_id,
            provider,
            token: token.into(),
            external_account_id,
            sync_status: SyncStatus::Pending,
            last_synced_at: None,
            created_at: Utc::now(),
        }
    }
}

#[async_trait::async_trait]
pub trait BankConnectionRepository: Send + Sync {
    async fn create(&self, conn: &BankConnection) -> anyhow::Result<()>;
    async fn find_by_id(&self, id: Uuid, user_id: Uuid) -> anyhow::Result<Option<BankConnection>>;
    async fn find_by_external_account_id(
        &self,
        provider: &BankProvider,
        external_account_id: &str,
    ) -> anyhow::Result<Option<BankConnection>>;
    async fn list_by_user(&self, user_id: Uuid) -> anyhow::Result<Vec<BankConnection>>;
    async fn list_incomplete(&self) -> anyhow::Result<Vec<BankConnection>>;
    async fn update_status(
        &self,
        id: Uuid,
        status: SyncStatus,
        last_synced_at: Option<DateTime<Utc>>,
    ) -> anyhow::Result<()>;
    async fn delete(&self, id: Uuid, user_id: Uuid) -> anyhow::Result<()>;
    /// True if any bank connection exists for the given account id. Used to detect
    /// externally-managed accounts (where balance is owned by the provider, not by us).
    async fn exists_for_account(&self, account_id: Uuid) -> anyhow::Result<bool>;
}
