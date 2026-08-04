use chrono::{DateTime, Utc};
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct UserSettings {
    pub user_id: Uuid,
    pub base_currency: String,
    pub updated_at: DateTime<Utc>,
}

impl UserSettings {
    pub fn default_for(user_id: Uuid) -> Self {
        Self {
            user_id,
            base_currency: "UAH".to_string(),
            updated_at: Utc::now(),
        }
    }
}

#[async_trait::async_trait]
pub trait UserSettingsRepository: Send + Sync {
    /// Returns `None` when the user has no row yet (defaults apply).
    async fn find(&self, user_id: Uuid) -> anyhow::Result<Option<UserSettings>>;

    async fn upsert(&self, settings: &UserSettings) -> anyhow::Result<()>;
}
