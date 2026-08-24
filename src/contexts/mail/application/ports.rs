//! Provider-neutral Mail ports.
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::Value;

use super::super::{
    domain::{ConnectionVersion, GmailConnectionId},
    public::{ConnectionView, MailFacadeError},
};
use crate::shared_kernel::UserId;

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

#[derive(Clone, Debug)]
pub(crate) struct OauthStartResult {
    pub response: Value,
    pub replayed: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct CallbackResult {
    pub response: Value,
    pub replayed: bool,
}

#[derive(Clone, Debug)]
pub(crate) enum OauthCallbackPreparation {
    Replay(CallbackResult),
    Exchange { verifier: String },
}

pub(crate) struct StartOauthRecord<'a> {
    pub user: UserId,
    pub replacement: Option<uuid::Uuid>,
    pub expected: Option<u64>,
    pub key: &'a str,
    pub hash: [u8; 32],
    pub now: DateTime<Utc>,
    pub oauth: &'a dyn GmailOAuth,
}

#[async_trait]
pub(crate) trait MailRepository: Send + Sync {
    async fn list_connections(&self, user: UserId) -> Result<Vec<ConnectionView>, MailFacadeError>;
    async fn get_connection(
        &self,
        user: UserId,
        id: GmailConnectionId,
    ) -> Result<Option<ConnectionView>, MailFacadeError>;
    async fn connection_status(
        &self,
        user: UserId,
        id: GmailConnectionId,
    ) -> Result<Option<Value>, MailFacadeError>;
    async fn disconnect_command(
        &self,
        user: UserId,
        id: uuid::Uuid,
        expected: u64,
        key: &str,
        hash: [u8; 32],
        now: DateTime<Utc>,
    ) -> Result<Value, MailFacadeError>;
    async fn resync_command(
        &self,
        user: UserId,
        id: uuid::Uuid,
        expected: u64,
        key: &str,
        hash: [u8; 32],
        now: DateTime<Utc>,
    ) -> Result<Value, MailFacadeError>;
    async fn start_oauth(
        &self,
        record: StartOauthRecord<'_>,
    ) -> Result<OauthStartResult, MailFacadeError>;
    async fn prepare_oauth_callback(
        &self,
        state: &str,
        code: &str,
        now: DateTime<Utc>,
    ) -> Result<OauthCallbackPreparation, MailFacadeError>;
    async fn complete_oauth(
        &self,
        state: &str,
        code: &str,
        tokens: OAuthTokens,
        now: DateTime<Utc>,
    ) -> Result<CallbackResult, MailFacadeError>;
    async fn record_oauth_provider_failure(
        &self,
        state: &str,
        code: &str,
        now: DateTime<Utc>,
    ) -> Result<(), MailFacadeError>;
}
