//! Stable contracts published by Mail.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use super::application::{
    commands::canonical_request_hash,
    ports::{GmailOAuth, MailRepository, OauthCallbackPreparation, StartOauthRecord},
};
use crate::shared_kernel::{CurrencyCode, Money, UserId};

pub use super::domain::{
    ConnectionState, ConnectionVersion, GmailConnectionId, MailError, SourceMessageId,
};
crate::define_uuid_id!(#[doc = "Identifies normalized receipt evidence."] pub ReceiptEvidenceId);

pub const CONTEXT_NAME: &str = "mail";
pub const RECEIPT_EVIDENCE_RECORDED_V1: &str = "mail.receipt-evidence-recorded.v1";

#[derive(Clone)]
pub struct MailFacade {
    repository: Arc<dyn MailRepository>,
    oauth: Arc<dyn GmailOAuth>,
}
impl MailFacade {
    pub(crate) fn new(repository: Arc<dyn MailRepository>, oauth: Arc<dyn GmailOAuth>) -> Self {
        Self { repository, oauth }
    }

    pub async fn start_oauth(
        &self,
        user: UserId,
        command: StartOauth,
        idempotency_key: &str,
        now: DateTime<Utc>,
    ) -> Result<serde_json::Value, MailFacadeError> {
        let body = serde_json::json!({
            "connection_id": command.replacement_connection_id,
            "expected_version": command.expected_version.map(ConnectionVersion::get),
        });
        let target = command
            .replacement_connection_id
            .map(|id| id.into_uuid().to_string());
        let hash = canonical_request_hash("gmail_oauth_start", target.as_deref(), user, &body)
            .map_err(MailFacadeError::invalid_request)?;
        self.repository
            .start_oauth(StartOauthRecord {
                user,
                replacement: command
                    .replacement_connection_id
                    .map(GmailConnectionId::into_uuid),
                expected: command.expected_version.map(ConnectionVersion::get),
                key: idempotency_key,
                hash,
                now,
                oauth: self.oauth.as_ref(),
            })
            .await
            .map(|result| result.response)
    }

    pub async fn complete_oauth_callback(
        &self,
        state: &str,
        code: &str,
        now: DateTime<Utc>,
    ) -> Result<serde_json::Value, MailFacadeError> {
        let preparation = self
            .repository
            .prepare_oauth_callback(state, code, now)
            .await?;
        let result = match preparation {
            OauthCallbackPreparation::Replay(result) => result,
            OauthCallbackPreparation::Exchange { verifier } => {
                let tokens = match self.oauth.exchange(code, &verifier).await {
                    Ok(tokens) => tokens,
                    Err(error) => {
                        let _ = self
                            .repository
                            .record_oauth_provider_failure(state, code, now)
                            .await;
                        return Err(MailFacadeError::oauth_provider_anyhow(error));
                    }
                };
                self.repository
                    .complete_oauth(state, code, tokens, now)
                    .await?
            }
        };
        Ok(result.response)
    }

    pub async fn list_connections(
        &self,
        user: UserId,
    ) -> Result<Vec<ConnectionView>, MailFacadeError> {
        self.repository.list_connections(user).await
    }

    pub async fn connection_status(
        &self,
        user: UserId,
        id: GmailConnectionId,
    ) -> Result<Option<serde_json::Value>, MailFacadeError> {
        self.repository.connection_status(user, id).await
    }

    pub async fn disconnect(
        &self,
        user: UserId,
        id: GmailConnectionId,
        expected: ConnectionVersion,
        idempotency_key: &str,
        now: DateTime<Utc>,
    ) -> Result<serde_json::Value, MailFacadeError> {
        let body = serde_json::json!({"expected_version": expected.get()});
        let target = id.into_uuid().to_string();
        let hash =
            canonical_request_hash("disconnect_email_connection", Some(&target), user, &body)
                .map_err(MailFacadeError::invalid_request)?;
        self.repository
            .disconnect_command(
                user,
                id.into_uuid(),
                expected.get(),
                idempotency_key,
                hash,
                now,
            )
            .await
    }

    pub async fn resync(
        &self,
        user: UserId,
        id: GmailConnectionId,
        expected: ConnectionVersion,
        idempotency_key: &str,
        now: DateTime<Utc>,
    ) -> Result<serde_json::Value, MailFacadeError> {
        let body = serde_json::json!({"expected_version": expected.get()});
        let target = id.into_uuid().to_string();
        let hash = canonical_request_hash("resync_email_connection", Some(&target), user, &body)
            .map_err(MailFacadeError::invalid_request)?;
        self.repository
            .resync_command(
                user,
                id.into_uuid(),
                expected.get(),
                idempotency_key,
                hash,
                now,
            )
            .await
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MailFacadeErrorKind {
    NotFound,
    VersionConflict,
    IdempotencyConflict,
    Invalid,
    OauthProvider,
    Persistence,
}

#[derive(Debug)]
pub struct MailFacadeError {
    kind: MailFacadeErrorKind,
    message: &'static str,
    source: Option<Box<dyn std::error::Error + Send + Sync>>,
}

impl MailFacadeError {
    pub(crate) fn not_found(source: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self::with_source(
            MailFacadeErrorKind::NotFound,
            "mail resource was not found",
            source,
        )
    }
    pub(crate) fn version_conflict(source: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self::with_source(
            MailFacadeErrorKind::VersionConflict,
            "mail connection version conflict",
            source,
        )
    }
    pub(crate) fn idempotency_conflict(
        source: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self::with_source(
            MailFacadeErrorKind::IdempotencyConflict,
            "mail idempotency conflict",
            source,
        )
    }
    pub(crate) fn invalid(source: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self::with_source(
            MailFacadeErrorKind::Invalid,
            "mail request is invalid",
            source,
        )
    }
    pub(crate) fn oauth_provider(source: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self::with_source(
            MailFacadeErrorKind::OauthProvider,
            "OAuth provider request failed",
            source,
        )
    }
    pub(crate) fn oauth_provider_anyhow(source: anyhow::Error) -> Self {
        Self {
            kind: MailFacadeErrorKind::OauthProvider,
            message: "OAuth provider request failed",
            source: Some(source.into_boxed_dyn_error()),
        }
    }
    pub(crate) fn storage(source: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self::with_source(
            MailFacadeErrorKind::Persistence,
            "mail storage is unavailable",
            source,
        )
    }
    pub(crate) fn invalid_request(source: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self::with_source(
            MailFacadeErrorKind::Invalid,
            "mail request could not be encoded",
            source,
        )
    }
    fn with_source(
        kind: MailFacadeErrorKind,
        message: &'static str,
        source: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self {
            kind,
            message,
            source: Some(Box::new(source)),
        }
    }
    pub fn is_not_found(&self) -> bool {
        self.kind == MailFacadeErrorKind::NotFound
    }
    pub fn is_conflict(&self) -> bool {
        matches!(
            self.kind,
            MailFacadeErrorKind::VersionConflict | MailFacadeErrorKind::IdempotencyConflict
        )
    }
    pub fn is_idempotency_conflict(&self) -> bool {
        self.kind == MailFacadeErrorKind::IdempotencyConflict
    }
    pub fn is_invalid(&self) -> bool {
        self.kind == MailFacadeErrorKind::Invalid
    }
    pub fn is_oauth_provider(&self) -> bool {
        self.kind == MailFacadeErrorKind::OauthProvider
    }
}

impl std::fmt::Display for MailFacadeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message)
    }
}
impl std::error::Error for MailFacadeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source.as_deref().map(|source| source as _)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReceiptEvidenceKind {
    Renewal,
    OneTime,
    Refund,
    Cancellation,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ReceiptEvidenceRecordedV1 {
    pub evidence_id: ReceiptEvidenceId,
    pub user_id: UserId,
    pub source_message_id: SourceMessageId,
    pub merchant: String,
    pub kind: ReceiptEvidenceKind,
    pub money: Option<Money>,
    pub charged_at: Option<DateTime<Utc>>,
    pub parser_name: String,
    pub parser_version: u32,
    pub provenance_digest: [u8; 32],
    pub recorded_at: DateTime<Utc>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectionView {
    pub id: GmailConnectionId,
    pub state: ConnectionState,
    pub version: ConnectionVersion,
    pub credential_generation: u64,
    pub sync_generation: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartOauth {
    pub replacement_connection_id: Option<GmailConnectionId>,
    pub expected_version: Option<ConnectionVersion>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReceiptEvidenceView {
    pub id: ReceiptEvidenceId,
    pub merchant: String,
    pub kind: ReceiptEvidenceKind,
    pub amount: Option<String>,
    pub currency: Option<CurrencyCode>,
    pub charged_at: Option<DateTime<Utc>>,
}
