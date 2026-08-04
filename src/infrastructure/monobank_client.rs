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
        let started = std::time::Instant::now();
        let resp = self
            .client
            .get(format!("{}/personal/client-info", self.base_url))
            .header("X-Token", token)
            .send()
            .await
            .context("monobank client-info request failed")?;
        let status = resp.status();
        let body = resp
            .text()
            .await
            .context("monobank client-info read body failed")?;
        let elapsed_ms = started.elapsed().as_millis();
        tracing::info!(
            endpoint = "/personal/client-info",
            %status,
            elapsed_ms,
            body = %body,
            "monobank request done"
        );
        if !status.is_success() {
            anyhow::bail!("monobank client-info non-2xx: {status}");
        }
        let info: ClientInfo = serde_json::from_str(&body)
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
        let started = std::time::Instant::now();
        let resp = self
            .client
            .get(format!(
                "{}/personal/statement/{account_id}/{from_ts}/{to_ts}",
                self.base_url
            ))
            .header("X-Token", token)
            .send()
            .await
            .context("monobank statement request failed")?;
        let status = resp.status();
        let body = resp
            .text()
            .await
            .context("monobank statement read body failed")?;
        let elapsed_ms = started.elapsed().as_millis();
        tracing::info!(
            endpoint = format!(
                "GET /personal/statement/{account_id}/{from_ts}/{to_ts}"),
            %status,
            elapsed_ms,
            body = %body,
            "monobank request done"
        );
        if !status.is_success() {
            anyhow::bail!("monobank statement non-2xx: {status}");
        }
        let items: Vec<MonoStatementItem> =
            serde_json::from_str(&body).context("monobank statement parse failed")?;
        Ok(items)
    }

    async fn set_webhook(&self, token: &str, webhook_url: &str) -> anyhow::Result<()> {
        let started = std::time::Instant::now();
        let resp = self
            .client
            .post(format!("{}/personal/webhook", self.base_url))
            .header("X-Token", token)
            .json(&serde_json::json!({ "webHookUrl": webhook_url }))
            .send()
            .await
            .context("monobank set-webhook request failed")?;
        let status = resp.status();
        let body = resp
            .text()
            .await
            .context("monobank set-webhook read body failed")?;
        let elapsed_ms = started.elapsed().as_millis();
        tracing::info!(
            endpoint = "/personal/webhook",
            %status,
            elapsed_ms,
            body = %body,
            "monobank request done"
        );
        if !status.is_success() {
            anyhow::bail!("monobank set-webhook non-2xx: {status}");
        }
        Ok(())
    }
}
