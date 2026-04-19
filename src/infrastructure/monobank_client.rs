use anyhow::Context;
use chrono::{DateTime, Utc};
use serde::Deserialize;

use crate::domain::monobank::{MonoAccount, MonoStatementItem, MonobankApiClient};

pub struct ReqwestMonobankClient {
    client: reqwest::Client,
    base_url: String,
}

impl ReqwestMonobankClient {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: "https://api.monobank.ua".to_string(),
        }
    }

    /// For tests or overriding the base URL.
    pub fn with_base_url(base_url: String) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url,
        }
    }
}

impl Default for ReqwestMonobankClient {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Deserialize)]
struct ClientInfo {
    accounts: Vec<MonoAccount>,
}

#[async_trait::async_trait]
impl MonobankApiClient for ReqwestMonobankClient {
    async fn get_accounts(&self, token: &str) -> anyhow::Result<Vec<MonoAccount>> {
        let info: ClientInfo = self
            .client
            .get(format!("{}/personal/client-info", self.base_url))
            .header("X-Token", token)
            .send()
            .await
            .context("monobank client-info request failed")?
            .error_for_status()
            .context("monobank client-info non-2xx")?
            .json()
            .await
            .context("monobank client-info parse failed")?;
        Ok(info.accounts)
    }

    async fn get_statement(
        &self,
        token: &str,
        account_id: &str,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> anyhow::Result<Vec<MonoStatementItem>> {
        let from_ts = from.timestamp();
        let to_ts = to.timestamp();
        let items: Vec<MonoStatementItem> = self
            .client
            .get(format!(
                "{}/personal/statement/{account_id}/{from_ts}/{to_ts}",
                self.base_url
            ))
            .header("X-Token", token)
            .send()
            .await
            .context("monobank statement request failed")?
            .error_for_status()
            .context("monobank statement non-2xx")?
            .json()
            .await
            .context("monobank statement parse failed")?;
        Ok(items)
    }

    async fn set_webhook(&self, token: &str, webhook_url: &str) -> anyhow::Result<()> {
        self.client
            .post(format!("{}/personal/webhook", self.base_url))
            .header("X-Token", token)
            .json(&serde_json::json!({ "webHookUrl": webhook_url }))
            .send()
            .await
            .context("monobank set-webhook request failed")?
            .error_for_status()
            .context("monobank set-webhook non-2xx")?;
        Ok(())
    }
}
