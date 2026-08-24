//! Mail application repository implemented by PostgreSQL.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::Value;

use super::{MailStoreError, PgMailStore};
use crate::{
    contexts::mail::{
        application::ports::{
            CallbackResult, GmailOAuth, MailRepository, OAuthTokens, OauthCallbackPreparation,
            OauthStartResult,
        },
        domain::GmailConnectionId,
        public::{ConnectionView, MailFacadeError},
    },
    shared_kernel::UserId,
};

fn store_error(error: MailStoreError) -> MailFacadeError {
    match error {
        error @ MailStoreError::NotFound => MailFacadeError::not_found(error),
        error @ MailStoreError::VersionConflict => MailFacadeError::version_conflict(error),
        error @ MailStoreError::IdempotencyConflict => MailFacadeError::idempotency_conflict(error),
        error @ MailStoreError::InvalidOauthState => MailFacadeError::invalid(error),
        error @ MailStoreError::OAuthProvider => MailFacadeError::oauth_provider(error),
        MailStoreError::Database(error) => MailFacadeError::storage(error),
    }
}

#[async_trait]
impl MailRepository for PgMailStore {
    async fn list_connections(&self, user: UserId) -> Result<Vec<ConnectionView>, MailFacadeError> {
        PgMailStore::list_connections(self, user)
            .await
            .map_err(MailFacadeError::storage)
    }

    async fn get_connection(
        &self,
        user: UserId,
        id: GmailConnectionId,
    ) -> Result<Option<ConnectionView>, MailFacadeError> {
        PgMailStore::get_connection(self, user, id)
            .await
            .map_err(MailFacadeError::storage)
    }

    async fn connection_status(
        &self,
        user: UserId,
        id: GmailConnectionId,
    ) -> Result<Option<Value>, MailFacadeError> {
        PgMailStore::connection_status(self, user, id)
            .await
            .map_err(MailFacadeError::storage)
    }

    async fn disconnect_command(
        &self,
        user: UserId,
        id: uuid::Uuid,
        expected: u64,
        key: &str,
        hash: [u8; 32],
        now: DateTime<Utc>,
    ) -> Result<Value, MailFacadeError> {
        PgMailStore::disconnect_command(self, user, id, expected, key, hash, now)
            .await
            .map_err(store_error)
    }

    async fn resync_command(
        &self,
        user: UserId,
        id: uuid::Uuid,
        expected: u64,
        key: &str,
        hash: [u8; 32],
        now: DateTime<Utc>,
    ) -> Result<Value, MailFacadeError> {
        PgMailStore::resync_command(self, user, id, expected, key, hash, now)
            .await
            .map_err(store_error)
    }

    async fn start_oauth(
        &self,
        record: super::super::application::ports::StartOauthRecord<'_>,
    ) -> Result<OauthStartResult, MailFacadeError> {
        PgMailStore::start_oauth(
            self,
            record.user,
            record.replacement,
            record.expected,
            record.key,
            record.hash,
            record.now,
            record.oauth,
        )
        .await
        .map_err(store_error)
    }

    async fn prepare_oauth_callback(
        &self,
        state: &str,
        code: &str,
        now: DateTime<Utc>,
    ) -> Result<OauthCallbackPreparation, MailFacadeError> {
        PgMailStore::prepare_oauth_callback(self, state, code, now)
            .await
            .map_err(store_error)
    }

    async fn complete_oauth(
        &self,
        state: &str,
        code: &str,
        tokens: OAuthTokens,
        now: DateTime<Utc>,
    ) -> Result<CallbackResult, MailFacadeError> {
        PgMailStore::complete_oauth(self, state, code, tokens, now)
            .await
            .map_err(store_error)
    }

    async fn record_oauth_provider_failure(
        &self,
        state: &str,
        code: &str,
        now: DateTime<Utc>,
    ) -> Result<(), MailFacadeError> {
        PgMailStore::record_oauth_provider_failure(self, state, code, now)
            .await
            .map_err(MailFacadeError::storage)
    }
}
