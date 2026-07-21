use chrono::{DateTime, Utc};

use crate::domain::email_connection::EmailConnection;

#[derive(Debug, Clone)]
pub struct RawEmail {
    /// Provider-scoped message identifier. Gmail IDs are only unique inside a
    /// mailbox, so persistence must combine this value with the connection id.
    pub provider_message_id: String,
    /// RFC 5322 Message-ID header, when present. Retained only for legacy
    /// deduplication and diagnostics; it is not the primary idempotency key.
    pub rfc_message_id: Option<String>,
    pub from: String,
    pub subject: String,
    pub authentication_results: Vec<String>,
    pub received_at: DateTime<Utc>,
    pub body_text: Option<String>,
    pub body_html: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct EmailFetchBatch {
    pub emails: Vec<RawEmail>,
    /// Message-local failures that can be retried without blocking the
    /// provider cursor. Authentication, network, and rate-limit failures are
    /// returned as `Err` instead.
    pub failures: Vec<EmailMessageFetchFailure>,
    /// Provider IDs that were fetched successfully but rejected before body
    /// download (for example, an untrusted Authentication-Results header).
    /// The application persists these as ignored so retries and cursors remain
    /// monotonic without storing message bodies.
    pub ignored_message_ids: Vec<String>,
    pub next_history_id: Option<String>,
    pub history_was_reset: bool,
}

#[derive(Debug, Clone)]
pub struct EmailMessageFetchFailure {
    pub provider_message_id: String,
    pub error_kind: String,
}

#[async_trait::async_trait]
pub trait EmailFetcher: Send + Sync {
    /// Fetch messages added since the connection cursor. Implementations must
    /// fall back to an overlapping bounded scan when the provider cursor has
    /// expired, and must not persist the returned cursor themselves.
    async fn fetch_new(&self, conn: &EmailConnection) -> anyhow::Result<EmailFetchBatch>;

    async fn fetch_by_ids(
        &self,
        _conn: &EmailConnection,
        _provider_message_ids: Vec<String>,
    ) -> anyhow::Result<EmailFetchBatch> {
        anyhow::bail!("fetching messages by provider id is not supported")
    }
}
