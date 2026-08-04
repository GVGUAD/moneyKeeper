use chrono::{DateTime, Utc};

use crate::domain::email_connection::EmailConnection;

#[derive(Debug, Clone)]
pub struct RawEmail {
    pub message_id: String,
    pub from: String,
    pub subject: String,
    pub received_at: DateTime<Utc>,
    pub body_text: Option<String>,
    pub body_html: Option<String>,
}

#[async_trait::async_trait]
pub trait EmailFetcher: Send + Sync {
    /// Fetch new emails since the connection's cursor. Returns the list of new
    /// emails plus the cursor value to persist back on the connection.
    async fn fetch_new(
        &self,
        conn: &EmailConnection,
    ) -> anyhow::Result<(Vec<RawEmail>, Option<String>)>;
}
