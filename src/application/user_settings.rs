use std::sync::Arc;

use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::fx_rate::FxRateRepository;
use crate::domain::user_settings::{UserSettings, UserSettingsRepository};

pub struct UserSettingsService {
    repo: Arc<dyn UserSettingsRepository>,
    fx_repo: Arc<dyn FxRateRepository>,
}

impl UserSettingsService {
    pub fn new(
        repo: Arc<dyn UserSettingsRepository>,
        fx_repo: Arc<dyn FxRateRepository>,
    ) -> Self {
        Self { repo, fx_repo }
    }

    pub async fn get_or_default(&self, user_id: Uuid) -> anyhow::Result<UserSettings> {
        match self.repo.find(user_id).await? {
            Some(s) => Ok(s),
            None => Ok(UserSettings::default_for(user_id)),
        }
    }

    pub async fn set_base_currency(
        &self,
        user_id: Uuid,
        base_currency: &str,
    ) -> anyhow::Result<UserSettings> {
        let normalized = base_currency.to_uppercase();
        if normalized.len() != 3 {
            return Err(DomainError::InvalidInput(
                "base_currency must be a 3-letter ISO code".to_string(),
            )
            .into());
        }
        if normalized != "UAH" {
            let known = self.fx_repo.known_currencies().await?;
            if !known.iter().any(|c| c == &normalized) {
                return Err(DomainError::InvalidInput(format!(
                    "unknown currency: {normalized}"
                ))
                .into());
            }
        }
        let mut s = self.get_or_default(user_id).await?;
        s.base_currency = normalized;
        self.repo.upsert(&s).await?;
        Ok(s)
    }
}
