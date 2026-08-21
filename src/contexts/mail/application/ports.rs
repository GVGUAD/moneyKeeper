//! Provider-neutral Mail ports.
use async_trait::async_trait;
use chrono::{DateTime, Utc};

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct OAuthTokens {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at: DateTime<Utc>,
}

impl std::fmt::Debug for OAuthTokens {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OAuthTokens")
            .field("access_token", &"[REDACTED]")
            .field(
                "refresh_token",
                &self.refresh_token.as_ref().map(|_| "[REDACTED]"),
            )
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

#[async_trait]
pub(crate) trait GmailOAuth: Send + Sync {
    fn authorization_url(&self, state: &str, challenge: &str) -> anyhow::Result<String>;
    async fn exchange(&self, code: &str, verifier: &str) -> anyhow::Result<OAuthTokens>;
    async fn refresh(&self, refresh_token: &str) -> anyhow::Result<OAuthTokens>;
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct GmailPage {
    pub messages: Vec<GmailMessage>,
    pub next_cursor: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct GmailMessage {
    pub provider_id: String,
    pub from: String,
    pub subject: String,
    pub body_text: Option<String>,
    pub body_html: Option<String>,
    pub received_at: chrono::DateTime<chrono::Utc>,
}
pub(crate) trait GmailSource: Send + Sync {
    fn fetch_page(
        &self,
        access_token: &str,
        cursor: Option<&str>,
    ) -> impl Future<Output = anyhow::Result<GmailPage>> + Send;
}
