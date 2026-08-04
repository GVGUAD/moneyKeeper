use async_trait::async_trait;
use chrono::Utc;
use sqlx::PgPool;
use uuid::Uuid;

use crate::domain::user_settings::{UserSettings, UserSettingsRepository};

pub struct PgUserSettingsRepository {
    pool: PgPool,
}

impl PgUserSettingsRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl UserSettingsRepository for PgUserSettingsRepository {
    async fn find(&self, user_id: Uuid) -> anyhow::Result<Option<UserSettings>> {
        let row = sqlx::query!(
            "SELECT user_id, base_currency, updated_at
             FROM user_settings
             WHERE user_id = $1",
            user_id,
        )
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(|r| UserSettings {
            user_id: r.user_id,
            base_currency: r.base_currency,
            updated_at: r.updated_at,
        }))
    }

    async fn upsert(&self, settings: &UserSettings) -> anyhow::Result<()> {
        let base = settings.base_currency.to_uppercase();
        sqlx::query!(
            r#"
            INSERT INTO user_settings (user_id, base_currency, updated_at)
            VALUES ($1, $2, $3)
            ON CONFLICT (user_id) DO UPDATE
            SET base_currency = EXCLUDED.base_currency,
                updated_at    = EXCLUDED.updated_at
            "#,
            settings.user_id,
            base,
            Utc::now(),
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}
