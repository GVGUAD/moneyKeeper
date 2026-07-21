use std::collections::HashSet;

use async_trait::async_trait;
use base64::Engine;
use chrono::{DateTime, Duration, Utc};
use regex::Regex;
use reqwest::StatusCode;
use serde::Deserialize;
use std::sync::OnceLock;
use tokio::task::JoinSet;

use crate::domain::email::{EmailFetchBatch, EmailFetcher, EmailMessageFetchFailure, RawEmail};
use crate::domain::email_connection::EmailConnection;
use crate::domain::secret::SecretString;

const GMAIL_API_BASE: &str = "https://gmail.googleapis.com/gmail/v1";
const SENDER_QUERY: &str =
    "from:(googleplay-noreply@google.com OR info@account.netflix.com OR no_reply@email.apple.com)";
const FETCH_CONCURRENCY: usize = 8;

#[derive(Clone)]
pub struct GmailClient {
    http: reqwest::Client,
    base_url: String,
    initial_lookback_days: i64,
}

impl GmailClient {
    pub fn production() -> Self {
        Self::with_base_url(GMAIL_API_BASE)
    }

    pub fn with_base_url(base_url: impl Into<String>) -> Self {
        let http = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .expect("valid Gmail HTTP client configuration");
        Self {
            http,
            base_url: base_url.into().trim_end_matches('/').to_string(),
            initial_lookback_days: 30,
        }
    }

    pub fn with_initial_lookback_days(mut self, days: i64) -> Self {
        self.initial_lookback_days = days.max(1);
        self
    }

    async fn profile_history_id(&self, access_token: &str) -> anyhow::Result<String> {
        let profile: ProfileResponse = self
            .http
            .get(format!("{}/users/me/profile", self.base_url))
            .bearer_auth(access_token)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(profile.history_id)
    }

    async fn list_messages(&self, access_token: &str, query: &str) -> anyhow::Result<Vec<String>> {
        let mut ids = Vec::new();
        let mut page_token: Option<String> = None;
        loop {
            let mut request = self
                .http
                .get(format!("{}/users/me/messages", self.base_url))
                .bearer_auth(access_token)
                .query(&[("q", query)]);
            if let Some(token) = page_token.as_deref() {
                request = request.query(&[("pageToken", token)]);
            }
            let response: MessagesListResponse =
                request.send().await?.error_for_status()?.json().await?;
            ids.extend(
                response
                    .messages
                    .unwrap_or_default()
                    .into_iter()
                    .map(|message| message.id),
            );
            page_token = response.next_page_token;
            if page_token.is_none() {
                break;
            }
        }
        Ok(ids)
    }

    async fn list_history(
        &self,
        access_token: &str,
        start_history_id: &str,
    ) -> anyhow::Result<HistoryResult> {
        let mut ids = Vec::new();
        let mut seen = HashSet::new();
        let mut page_token: Option<String> = None;
        let mut next_history_id = start_history_id.to_string();

        loop {
            let mut request = self
                .http
                .get(format!("{}/users/me/history", self.base_url))
                .bearer_auth(access_token)
                .query(&[
                    ("startHistoryId", start_history_id),
                    ("historyTypes", "messageAdded"),
                ]);
            if let Some(token) = page_token.as_deref() {
                request = request.query(&[("pageToken", token)]);
            }
            let response = request.send().await?;
            if response.status() == StatusCode::NOT_FOUND {
                return Ok(HistoryResult::Expired);
            }
            let page: HistoryListResponse = response.error_for_status()?.json().await?;
            if let Some(history_id) = page.history_id {
                next_history_id = history_id;
            }
            for record in page.history.unwrap_or_default() {
                for added in record.messages_added.unwrap_or_default() {
                    if seen.insert(added.message.id.clone()) {
                        ids.push(added.message.id);
                    }
                }
            }
            page_token = page.next_page_token;
            if page_token.is_none() {
                break;
            }
        }

        Ok(HistoryResult::Changes {
            message_ids: ids,
            next_history_id,
        })
    }

    async fn fetch_metadata(
        &self,
        access_token: &str,
        message_id: &str,
    ) -> anyhow::Result<MessageFull> {
        Ok(self
            .http
            .get(format!("{}/users/me/messages/{message_id}", self.base_url))
            .bearer_auth(access_token)
            .query(&[
                ("format", "metadata"),
                ("metadataHeaders", "From"),
                ("metadataHeaders", "Authentication-Results"),
            ])
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?)
    }

    async fn fetch_full_message(
        &self,
        access_token: &str,
        message_id: &str,
    ) -> anyhow::Result<MessageFull> {
        Ok(self
            .http
            .get(format!("{}/users/me/messages/{message_id}", self.base_url))
            .bearer_auth(access_token)
            .query(&[("format", "full")])
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?)
    }

    async fn attachment(
        &self,
        access_token: &str,
        message_id: &str,
        attachment_id: &str,
    ) -> anyhow::Result<Option<String>> {
        let response: AttachmentResponse = self
            .http
            .get(format!(
                "{}/users/me/messages/{message_id}/attachments/{attachment_id}",
                self.base_url
            ))
            .bearer_auth(access_token)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        response.data.as_deref().map(decode_base64url).transpose()
    }

    async fn message_to_raw(
        &self,
        access_token: &str,
        msg: MessageFull,
    ) -> anyhow::Result<RawEmail> {
        let from = header(&msg.payload, "From").unwrap_or_default();
        let subject = header(&msg.payload, "Subject").unwrap_or_default();
        let rfc_message_id =
            header(&msg.payload, "Message-ID").or_else(|| header(&msg.payload, "Message-Id"));
        let authentication_results = headers(&msg.payload, "Authentication-Results");
        let received_at = msg
            .internal_date
            .parse::<i64>()
            .ok()
            .and_then(DateTime::<Utc>::from_timestamp_millis)
            .ok_or_else(|| anyhow::anyhow!("invalid Gmail internalDate for {}", msg.id))?;

        let mut parts = Vec::new();
        collect_body_parts(&msg.payload, &mut parts);
        let mut text = None;
        let mut html = None;
        for part in parts {
            let decoded = if let Some(data) = part.data.as_deref() {
                Some(decode_base64url(data)?)
            } else if let Some(attachment_id) = part.attachment_id.as_deref() {
                self.attachment(access_token, &msg.id, attachment_id)
                    .await?
            } else {
                None
            };
            match part.mime_type.as_str() {
                "text/plain" if text.is_none() => text = decoded,
                "text/html" if html.is_none() => html = decoded,
                _ => {}
            }
        }

        Ok(RawEmail {
            provider_message_id: msg.id,
            rfc_message_id,
            from,
            subject,
            authentication_results,
            received_at,
            body_text: text,
            body_html: html,
        })
    }

    async fn fetch_relevant_message(
        &self,
        access_token: &str,
        message_id: &str,
    ) -> anyhow::Result<Option<RawEmail>> {
        let metadata = self.fetch_metadata(access_token, message_id).await?;
        let from = header(&metadata.payload, "From").unwrap_or_default();
        let auth = headers(&metadata.payload, "Authentication-Results");
        if !is_trusted_authenticated_sender(&from, &auth) {
            return Ok(None);
        }
        let full = self.fetch_full_message(access_token, message_id).await?;
        self.message_to_raw(access_token, full).await.map(Some)
    }

    async fn fetch_messages(
        &self,
        access_token: &SecretString,
        message_ids: Vec<String>,
    ) -> anyhow::Result<(Vec<RawEmail>, Vec<EmailMessageFetchFailure>, Vec<String>)> {
        let mut output = Vec::new();
        let mut failures = Vec::new();
        let mut ignored_message_ids = Vec::new();
        let mut ids = message_ids.into_iter();
        let mut tasks = JoinSet::new();
        let access_token = std::sync::Arc::new(access_token.clone());

        loop {
            while tasks.len() < FETCH_CONCURRENCY {
                let Some(id) = ids.next() else { break };
                let client = self.clone();
                let token = std::sync::Arc::clone(&access_token);
                tasks.spawn(async move {
                    let result = client.fetch_relevant_message(token.expose(), &id).await;
                    (id, result)
                });
            }
            if tasks.is_empty() {
                break;
            }
            match tasks.join_next().await {
                Some(Ok((_id, Ok(Some(email))))) => output.push(email),
                Some(Ok((id, Ok(None)))) => ignored_message_ids.push(id),
                Some(Ok((_id, Err(error)))) if is_connection_level_error(&error) => {
                    return Err(error);
                }
                Some(Ok((id, Err(_error)))) => failures.push(EmailMessageFetchFailure {
                    provider_message_id: id,
                    error_kind: "message_fetch_failed".to_string(),
                }),
                Some(Err(error)) => return Err(error.into()),
                None => break,
            }
        }
        output.sort_by_key(|email| email.received_at);
        ignored_message_ids.sort();
        Ok((output, failures, ignored_message_ids))
    }
}

#[async_trait]
impl EmailFetcher for GmailClient {
    async fn fetch_new(&self, conn: &EmailConnection) -> anyhow::Result<EmailFetchBatch> {
        let access_token = &conn.oauth_access_token;
        let (message_ids, next_history_id, history_was_reset) =
            match conn.last_history_id.as_deref() {
                Some(cursor) => match self.list_history(access_token, cursor).await? {
                    HistoryResult::Changes {
                        message_ids,
                        next_history_id,
                    } => (message_ids, next_history_id, false),
                    HistoryResult::Expired => {
                        let baseline = self.profile_history_id(access_token).await?;
                        let query = fallback_query(conn.last_synced_at, self.initial_lookback_days);
                        (
                            self.list_messages(access_token, &query).await?,
                            baseline,
                            true,
                        )
                    }
                },
                None => {
                    // Establish the cursor before listing. Messages arriving after
                    // this point will be returned by the next history request.
                    let baseline = self.profile_history_id(access_token).await?;
                    let query =
                        format!("{SENDER_QUERY} newer_than:{}d", self.initial_lookback_days);
                    (
                        self.list_messages(access_token, &query).await?,
                        baseline,
                        false,
                    )
                }
            };

        let (emails, failures, ignored_message_ids) =
            self.fetch_messages(access_token, message_ids).await?;
        Ok(EmailFetchBatch {
            emails,
            failures,
            ignored_message_ids,
            next_history_id: Some(next_history_id),
            history_was_reset,
        })
    }

    async fn fetch_by_ids(
        &self,
        conn: &EmailConnection,
        provider_message_ids: Vec<String>,
    ) -> anyhow::Result<EmailFetchBatch> {
        let (emails, failures, ignored_message_ids) = self
            .fetch_messages(&conn.oauth_access_token, provider_message_ids)
            .await?;
        Ok(EmailFetchBatch {
            emails,
            failures,
            ignored_message_ids,
            next_history_id: conn.last_history_id.clone(),
            history_was_reset: false,
        })
    }
}

fn is_connection_level_error(error: &anyhow::Error) -> bool {
    let Some(error) = error.downcast_ref::<reqwest::Error>() else {
        return false;
    };
    if error.is_timeout() || error.is_connect() || error.is_request() || error.is_body() {
        return true;
    }
    match error.status() {
        Some(StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN | StatusCode::TOO_MANY_REQUESTS) => {
            true
        }
        Some(status) if status.is_server_error() => true,
        _ => false,
    }
}

fn fallback_query(last_synced_at: Option<DateTime<Utc>>, lookback_days: i64) -> String {
    match last_synced_at {
        Some(last_sync) => format!(
            "{SENDER_QUERY} after:{}",
            (last_sync - Duration::hours(24)).timestamp()
        ),
        None => format!("{SENDER_QUERY} newer_than:{lookback_days}d"),
    }
}

fn extract_mailbox(from: &str) -> Option<String> {
    let trimmed = from.trim();
    if trimmed.is_empty() {
        return None;
    }

    let mut quoted = false;
    let mut escaped = false;
    let mut comment_depth = 0_u32;
    let mut angle_start = None;
    let mut angle_end = None;
    for (index, character) in trimmed.char_indices() {
        if quoted {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                quoted = false;
            }
            continue;
        }
        if comment_depth > 0 {
            if escaped {
                escaped = false;
            } else {
                match character {
                    '\\' => escaped = true,
                    '(' => comment_depth += 1,
                    ')' => comment_depth -= 1,
                    _ => {}
                }
            }
            continue;
        }
        match character {
            '"' => quoted = true,
            '(' => comment_depth = 1,
            '<' if angle_start.is_none() && angle_end.is_none() => angle_start = Some(index),
            '>' if angle_start.is_some() && angle_end.is_none() => angle_end = Some(index),
            '<' | '>' | ',' | ':' | ';' => return None,
            _ => {}
        }
    }
    if quoted || comment_depth != 0 {
        return None;
    }

    match (angle_start, angle_end) {
        (None, None) => Some(trimmed.to_ascii_lowercase()),
        (Some(start), Some(end)) if start < end => {
            let prefix = strip_quoted_strings_and_comments(&trimmed[..start])?;
            if prefix
                .chars()
                .any(|character| matches!(character, '@' | '<' | '>' | ',' | ':' | ';'))
                || !is_cfws(&trimmed[end + 1..])
            {
                return None;
            }
            Some(trimmed[start + 1..end].trim().to_ascii_lowercase())
        }
        _ => None,
    }
}

fn is_trusted_authenticated_sender(from: &str, authentication_results: &[String]) -> bool {
    let Some(mailbox) = extract_mailbox(from) else {
        return false;
    };
    let signing_domains: &[&str] = match mailbox.as_str() {
        "googleplay-noreply@google.com" => &["google.com"],
        "info@account.netflix.com" => &["netflix.com"],
        "no_reply@email.apple.com" => &["apple.com", "email.apple.com"],
        _ => return false,
    };
    authentication_results.iter().any(|result| {
        let Some((authserv_id, clauses)) = result.split_once(';') else {
            return false;
        };
        if !authserv_id.trim().eq_ignore_ascii_case("mx.google.com") {
            return false;
        }
        let Some(sanitized) = strip_quoted_strings_and_comments(clauses) else {
            return false;
        };
        sanitized.split(';').any(|clause| {
            let Some(signing_domain) = authenticated_sender_domain(clause) else {
                return false;
            };
            signing_domains.iter().any(|allowed| {
                signing_domain == *allowed || signing_domain.ends_with(&format!(".{allowed}"))
            })
        })
    })
}

// Authentication-Results properties may contain attacker-controlled quoted
// strings and comments. Strip them before splitting method clauses so a
// semicolon inside one cannot smuggle a synthetic dkim=pass or dmarc=pass
// clause. Malformed, unterminated values invalidate the entire header.
fn strip_quoted_strings_and_comments(value: &str) -> Option<String> {
    let mut sanitized = String::with_capacity(value.len());
    let mut quoted = false;
    let mut escaped = false;
    let mut comment_depth = 0_u32;
    for character in value.chars() {
        if quoted {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                quoted = false;
            }
            sanitized.push(' ');
            continue;
        }
        if comment_depth > 0 {
            if escaped {
                escaped = false;
            } else {
                match character {
                    '\\' => escaped = true,
                    '(' => comment_depth += 1,
                    ')' => comment_depth -= 1,
                    _ => {}
                }
            }
            sanitized.push(' ');
            continue;
        }
        match character {
            '"' => {
                quoted = true;
                sanitized.push(' ');
            }
            '(' => {
                comment_depth = 1;
                sanitized.push(' ');
            }
            _ => sanitized.push(character),
        }
    }
    (!quoted && comment_depth == 0).then_some(sanitized)
}

fn is_cfws(value: &str) -> bool {
    let mut comment_depth = 0_u32;
    let mut escaped = false;
    for character in value.chars() {
        if comment_depth > 0 {
            if escaped {
                escaped = false;
            } else {
                match character {
                    '\\' => escaped = true,
                    '(' => comment_depth += 1,
                    ')' => comment_depth -= 1,
                    _ => {}
                }
            }
        } else {
            match character {
                '(' => comment_depth = 1,
                character if character.is_whitespace() => {}
                _ => return false,
            }
        }
    }
    comment_depth == 0
}

fn authenticated_sender_domain(clause: &str) -> Option<String> {
    if dkim_pass_regex().is_match(clause) {
        let mut header_d = None;
        let mut header_i = None;
        for capture in dkim_identity_regex().captures_iter(clause) {
            let property = capture.get(1)?.as_str();
            let value = capture.get(2)?.as_str();
            let domain = if property.eq_ignore_ascii_case("d") {
                normalize_domain(value)?
            } else {
                normalize_dkim_identity_domain(value)?
            };
            let destination = if property.eq_ignore_ascii_case("d") {
                &mut header_d
            } else {
                &mut header_i
            };
            if destination.replace(domain).is_some() {
                return None;
            }
        }

        return match (header_d, header_i) {
            (Some(header_d), Some(header_i)) if header_d == header_i => Some(header_d),
            (Some(header_d), None) => Some(header_d),
            (None, Some(header_i)) => Some(header_i),
            _ => None,
        };
    }

    if dmarc_pass_regex().is_match(clause) {
        let mut domains = dmarc_from_regex()
            .captures_iter(clause)
            .map(|capture| normalize_domain(capture.get(1)?.as_str()))
            .collect::<Option<Vec<_>>>()?;
        return (domains.len() == 1).then(|| domains.remove(0));
    }

    None
}

fn normalize_dkim_identity_domain(identity: &str) -> Option<String> {
    let (local_part, domain) = identity.split_once('@')?;
    if local_part.contains('@') || domain.contains('@') {
        return None;
    }
    normalize_domain(domain)
}

fn normalize_domain(domain: &str) -> Option<String> {
    if domain.is_empty() || domain.len() > 253 || !domain.is_ascii() {
        return None;
    }
    for label in domain.split('.') {
        if label.is_empty()
            || label.len() > 63
            || !label
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            || !label
                .as_bytes()
                .first()
                .is_some_and(u8::is_ascii_alphanumeric)
            || !label
                .as_bytes()
                .last()
                .is_some_and(u8::is_ascii_alphanumeric)
        {
            return None;
        }
    }
    Some(domain.to_ascii_lowercase())
}

fn dkim_pass_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(r"(?i)^\s*dkim\s*=\s*pass(?:\s|$)").expect("valid DKIM pass regex")
    })
}

fn dmarc_pass_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(r"(?i)^\s*dmarc\s*=\s*pass(?:\s|$)").expect("valid DMARC pass regex")
    })
}

fn dkim_identity_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(r"(?i)(?:^|\s)header\.(d|i)\s*=\s*(\S+)").expect("valid DKIM identity regex")
    })
}

fn dmarc_from_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(r"(?i)(?:^|\s)header\.from\s*=\s*(\S+)").expect("valid DMARC From-domain regex")
    })
}

fn decode_base64url(value: &str) -> anyhow::Result<String> {
    let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(value)
        .or_else(|_| base64::engine::general_purpose::URL_SAFE.decode(value))?;
    Ok(String::from_utf8(decoded)?)
}

fn header(payload: &Payload, name: &str) -> Option<String> {
    payload
        .headers
        .iter()
        .find(|header| header.name.eq_ignore_ascii_case(name))
        .map(|header| header.value.clone())
}

fn headers(payload: &Payload, name: &str) -> Vec<String> {
    payload
        .headers
        .iter()
        .filter(|header| header.name.eq_ignore_ascii_case(name))
        .map(|header| header.value.clone())
        .collect()
}

fn collect_body_parts(payload: &Payload, output: &mut Vec<BodyPart>) {
    if matches!(payload.mime_type.as_str(), "text/plain" | "text/html") {
        output.push(BodyPart {
            mime_type: payload.mime_type.clone(),
            data: payload.body.as_ref().and_then(|body| body.data.clone()),
            attachment_id: payload
                .body
                .as_ref()
                .and_then(|body| body.attachment_id.clone()),
        });
    }
    for part in payload.parts.as_deref().unwrap_or_default() {
        collect_body_parts(part, output);
    }
}

enum HistoryResult {
    Changes {
        message_ids: Vec<String>,
        next_history_id: String,
    },
    Expired,
}

#[derive(Deserialize)]
struct ProfileResponse {
    #[serde(rename = "historyId")]
    history_id: String,
}

#[derive(Deserialize)]
struct MessagesListResponse {
    messages: Option<Vec<MessageRef>>,
    #[serde(rename = "nextPageToken")]
    next_page_token: Option<String>,
}

#[derive(Clone, Deserialize)]
struct MessageRef {
    id: String,
}

#[derive(Deserialize)]
struct HistoryListResponse {
    history: Option<Vec<HistoryRecord>>,
    #[serde(rename = "nextPageToken")]
    next_page_token: Option<String>,
    #[serde(rename = "historyId")]
    history_id: Option<String>,
}

#[derive(Deserialize)]
struct HistoryRecord {
    #[serde(rename = "messagesAdded")]
    messages_added: Option<Vec<MessageAdded>>,
}

#[derive(Deserialize)]
struct MessageAdded {
    message: MessageRef,
}

#[derive(Deserialize)]
struct MessageFull {
    id: String,
    #[serde(default)]
    payload: Payload,
    #[serde(rename = "internalDate", default)]
    internal_date: String,
}

#[derive(Default, Deserialize)]
struct Payload {
    #[serde(default)]
    headers: Vec<Header>,
    body: Option<Body>,
    parts: Option<Vec<Payload>>,
    #[serde(rename = "mimeType", default)]
    mime_type: String,
}

#[derive(Deserialize)]
struct Header {
    name: String,
    value: String,
}

#[derive(Deserialize)]
struct Body {
    data: Option<String>,
    #[serde(rename = "attachmentId")]
    attachment_id: Option<String>,
}

struct BodyPart {
    mime_type: String,
    data: Option<String>,
    attachment_id: Option<String>,
}

#[derive(Deserialize)]
struct AttachmentResponse {
    data: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use axum::extract::{Path, Query, State};
    use axum::http::HeaderMap;
    use axum::response::{IntoResponse, Response};
    use axum::routing::get;
    use axum::{Json, Router};
    use serde_json::{Value, json};
    use tokio::net::TcpListener;
    use tokio::task::JoinHandle;
    use uuid::Uuid;

    use crate::domain::email_connection::{EmailConnectionStatus, EmailProvider};

    #[derive(Clone)]
    struct FakeResponse {
        status: StatusCode,
        body: Value,
    }

    impl FakeResponse {
        fn ok(body: Value) -> Self {
            Self {
                status: StatusCode::OK,
                body,
            }
        }

        fn status(status: StatusCode) -> Self {
            Self {
                status,
                body: json!({"error": status.as_u16()}),
            }
        }

        fn into_response(self) -> Response {
            (self.status, Json(self.body)).into_response()
        }
    }

    #[derive(Clone, Debug)]
    struct FakeCall {
        kind: String,
        query: HashMap<String, String>,
        authorization: Option<String>,
    }

    struct FakeGmail {
        profile: FakeResponse,
        message_pages: HashMap<String, FakeResponse>,
        history_pages: HashMap<String, FakeResponse>,
        metadata: HashMap<String, FakeResponse>,
        full_messages: HashMap<String, FakeResponse>,
        attachments: HashMap<(String, String), FakeResponse>,
        calls: Mutex<Vec<FakeCall>>,
    }

    impl FakeGmail {
        fn new(history_id: &str) -> Self {
            Self {
                profile: FakeResponse::ok(json!({"historyId": history_id})),
                message_pages: HashMap::new(),
                history_pages: HashMap::new(),
                metadata: HashMap::new(),
                full_messages: HashMap::new(),
                attachments: HashMap::new(),
                calls: Mutex::new(Vec::new()),
            }
        }

        fn record(
            &self,
            kind: impl Into<String>,
            query: HashMap<String, String>,
            headers: &HeaderMap,
        ) {
            self.calls.lock().unwrap().push(FakeCall {
                kind: kind.into(),
                query,
                authorization: headers
                    .get(axum::http::header::AUTHORIZATION)
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_string),
            });
        }

        fn response_or_not_found(response: Option<&FakeResponse>) -> Response {
            response
                .cloned()
                .unwrap_or_else(|| FakeResponse::status(StatusCode::NOT_FOUND))
                .into_response()
        }
    }

    async fn fake_profile(State(state): State<Arc<FakeGmail>>, headers: HeaderMap) -> Response {
        state.record("profile", HashMap::new(), &headers);
        state.profile.clone().into_response()
    }

    async fn fake_list_messages(
        State(state): State<Arc<FakeGmail>>,
        Query(query): Query<HashMap<String, String>>,
        headers: HeaderMap,
    ) -> Response {
        state.record("messages", query.clone(), &headers);
        let page = query.get("pageToken").cloned().unwrap_or_default();
        FakeGmail::response_or_not_found(state.message_pages.get(&page))
    }

    async fn fake_history(
        State(state): State<Arc<FakeGmail>>,
        Query(query): Query<HashMap<String, String>>,
        headers: HeaderMap,
    ) -> Response {
        state.record("history", query.clone(), &headers);
        let page = query.get("pageToken").cloned().unwrap_or_default();
        FakeGmail::response_or_not_found(state.history_pages.get(&page))
    }

    async fn fake_message(
        State(state): State<Arc<FakeGmail>>,
        Path(message_id): Path<String>,
        Query(query): Query<HashMap<String, String>>,
        headers: HeaderMap,
    ) -> Response {
        let format = query.get("format").cloned().unwrap_or_default();
        state.record(format!("message:{format}:{message_id}"), query, &headers);
        match format.as_str() {
            "metadata" => FakeGmail::response_or_not_found(state.metadata.get(&message_id)),
            "full" => FakeGmail::response_or_not_found(state.full_messages.get(&message_id)),
            _ => FakeResponse::status(StatusCode::BAD_REQUEST).into_response(),
        }
    }

    async fn fake_attachment(
        State(state): State<Arc<FakeGmail>>,
        Path((message_id, attachment_id)): Path<(String, String)>,
        headers: HeaderMap,
    ) -> Response {
        state.record(
            format!("attachment:{message_id}:{attachment_id}"),
            HashMap::new(),
            &headers,
        );
        FakeGmail::response_or_not_found(state.attachments.get(&(message_id, attachment_id)))
    }

    async fn spawn_fake(state: FakeGmail) -> (GmailClient, Arc<FakeGmail>, JoinHandle<()>) {
        let state = Arc::new(state);
        let app = Router::new()
            .route("/users/me/profile", get(fake_profile))
            .route("/users/me/messages", get(fake_list_messages))
            .route("/users/me/history", get(fake_history))
            .route("/users/me/messages/{message_id}", get(fake_message))
            .route(
                "/users/me/messages/{message_id}/attachments/{attachment_id}",
                get(fake_attachment),
            )
            .with_state(state.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (
            GmailClient::with_base_url(format!("http://{address}")),
            state,
            task,
        )
    }

    fn connection(
        last_history_id: Option<&str>,
        last_synced_at: Option<DateTime<Utc>>,
    ) -> EmailConnection {
        let now = Utc::now();
        EmailConnection {
            id: Uuid::new_v4(),
            user_id: Uuid::new_v4(),
            provider: EmailProvider::Gmail,
            email_address: "owner@example.com".to_string(),
            oauth_access_token: "test-access-token".into(),
            oauth_refresh_token: "test-refresh-token".into(),
            credential_version: 0,
            access_token_expires_at: now + Duration::hours(1),
            status: EmailConnectionStatus::Connected,
            last_synced_at,
            last_history_id: last_history_id.map(str::to_string),
            created_at: now,
        }
    }

    fn trusted_metadata(message_id: &str) -> Value {
        json!({
            "id": message_id,
            "payload": {
                "headers": [
                    {"name": "From", "value": "Netflix <info@account.netflix.com>"},
                    {
                        "name": "Authentication-Results",
                        "value": "mx.google.com; dkim=pass header.d=netflix.com"
                    }
                ]
            }
        })
    }

    fn full_text_message(message_id: &str, received_at_ms: i64, encoded_body: &str) -> Value {
        json!({
            "id": message_id,
            "internalDate": received_at_ms.to_string(),
            "payload": {
                "mimeType": "multipart/alternative",
                "headers": [
                    {"name": "From", "value": "Netflix <info@account.netflix.com>"},
                    {"name": "Subject", "value": "Your Netflix receipt"},
                    {"name": "Message-ID", "value": format!("<{message_id}@mail.example>")},
                    {
                        "name": "Authentication-Results",
                        "value": "mx.google.com; dkim=pass header.d=netflix.com"
                    }
                ],
                "parts": [{
                    "mimeType": "multipart/mixed",
                    "parts": [{
                        "mimeType": "text/plain",
                        "body": {"data": encoded_body}
                    }]
                }]
            }
        })
    }

    #[test]
    fn accepts_unpadded_base64url() {
        assert_eq!(decode_base64url("aGVsbG8").unwrap(), "hello");
    }

    #[test]
    fn sender_requires_exact_mailbox_and_dkim() {
        assert!(is_trusted_authenticated_sender(
            "Netflix <info@account.netflix.com>",
            &["mx.google.com; dkim=pass header.d=netflix.com".into()]
        ));
        assert!(!is_trusted_authenticated_sender(
            "attacker@account.netflix.com.evil.test",
            &["dkim=pass header.d=evil.test".into()]
        ));
        assert!(!is_trusted_authenticated_sender(
            "info@account.netflix.com",
            &["dkim=fail header.d=netflix.com".into()]
        ));
        assert!(!is_trusted_authenticated_sender(
            "info@account.netflix.com",
            &["dkim=pass header.d=netflix.com.evil.test".into()]
        ));
        assert!(!is_trusted_authenticated_sender(
            "info@account.netflix.com",
            &["dkim=pass header.d=evil.test; dkim=fail header.d=netflix.com".into()]
        ));
        assert!(!is_trusted_authenticated_sender(
            "info@account.netflix.com",
            &["attacker.example; dkim=pass header.d=netflix.com".into()]
        ));
        assert!(!is_trusted_authenticated_sender(
            "info@account.netflix.com",
            &["mx.google.com; spf=pass smtp.mailfrom=\"x dkim=pass header.d=netflix.com\"@evil.test".into()]
        ));
        assert!(!is_trusted_authenticated_sender(
            "info@account.netflix.com",
            &["mx.google.com; dkim=pass (header.d=netflix.com) header.d=evil.test".into()]
        ));
        assert!(!is_trusted_authenticated_sender(
            "Netflix <info@account.netflix.com>, attacker@evil.test",
            &["mx.google.com; dkim=pass header.d=netflix.com".into()]
        ));
        assert!(!is_trusted_authenticated_sender(
            "attacker@evil.test Netflix <info@account.netflix.com>",
            &["mx.google.com; dkim=pass header.d=netflix.com".into()]
        ));
    }

    #[test]
    fn accepts_real_google_play_gmail_authentication_results() {
        let from = "Google Play <googleplay-noreply@google.com>";
        let realistic = concat!(
            "mx.google.com; dkim=pass header.i=@google.com ",
            "header.s=20230601 header.b=AbCdEf; ",
            "spf=pass (google.com: domain of googleplay-noreply@google.com designates ",
            "209.85.220.69 as permitted sender) smtp.mailfrom=googleplay-noreply@google.com; ",
            "dmarc=pass (p=REJECT sp=REJECT dis=NONE) header.from=google.com"
        );
        assert!(is_trusted_authenticated_sender(
            from,
            &[realistic.to_string()]
        ));

        assert!(is_trusted_authenticated_sender(
            from,
            &["mx.google.com; dkim=pass header.i=@google.com header.s=20230601".into()]
        ));
        assert!(is_trusted_authenticated_sender(
            from,
            &["mx.google.com; dmarc=pass (p=REJECT dis=NONE) header.from=google.com".into()]
        ));
    }

    #[test]
    fn rejects_conflicting_or_smuggled_authentication_domains() {
        let from = "Google Play <googleplay-noreply@google.com>";
        for authentication_results in [
            "mx.google.com; dkim=pass header.d=evil.test header.i=@google.com",
            "mx.google.com; dkim=pass header.i=@google.com header.i=@evil.test",
            "mx.google.com; dmarc=pass header.from=google.com header.from=evil.test",
            "mx.google.com; spf=pass smtp.mailfrom=\"attacker; dkim=pass header.i=@google.com\"",
            "mx.google.com; spf=pass (attacker; dmarc=pass header.from=google.com)",
            "mx.google.com; dkim=pass header.i=\"@google.com\"",
            "mx.google.com; dkim=pass header.i=@google.com.evil.test",
            "attacker.example; dkim=pass header.i=@google.com",
        ] {
            assert!(
                !is_trusted_authenticated_sender(from, &[authentication_results.into()]),
                "unexpectedly trusted: {authentication_results}"
            );
        }
    }

    #[test]
    fn fallback_overlaps_previous_sync_by_a_day() {
        let last = DateTime::parse_from_rfc3339("2026-05-20T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let query = fallback_query(Some(last), 30);
        assert!(query.contains(&format!(
            "after:{}",
            (last - Duration::hours(24)).timestamp()
        )));
    }

    #[tokio::test]
    async fn initial_sync_paginates_and_reads_nested_inline_and_attachment_bodies() {
        let mut fake = FakeGmail::new("baseline-100");
        fake.message_pages.insert(
            String::new(),
            FakeResponse::ok(json!({
                "messages": [{"id": "later"}],
                "nextPageToken": "page-2"
            })),
        );
        fake.message_pages.insert(
            "page-2".to_string(),
            FakeResponse::ok(json!({"messages": [{"id": "earlier"}]})),
        );
        for id in ["later", "earlier"] {
            fake.metadata
                .insert(id.to_string(), FakeResponse::ok(trusted_metadata(id)));
        }

        let inline =
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode("inline unpadded body");
        fake.full_messages.insert(
            "later".to_string(),
            FakeResponse::ok(full_text_message("later", 1_700_000_002_000, &inline)),
        );
        fake.full_messages.insert(
            "earlier".to_string(),
            FakeResponse::ok(json!({
                "id": "earlier",
                "internalDate": "1700000001000",
                "payload": {
                    "mimeType": "multipart/mixed",
                    "headers": [
                        {"name": "From", "value": "Netflix <info@account.netflix.com>"},
                        {"name": "Subject", "value": "Attached receipt"},
                        {
                            "name": "Authentication-Results",
                            "value": "mx.google.com; dkim=pass header.d=netflix.com"
                        }
                    ],
                    "parts": [{
                        "mimeType": "multipart/alternative",
                        "parts": [{
                            "mimeType": "text/html",
                            "body": {"attachmentId": "body-attachment"}
                        }]
                    }]
                }
            })),
        );
        fake.attachments.insert(
            ("earlier".to_string(), "body-attachment".to_string()),
            FakeResponse::ok(json!({
                "data": base64::engine::general_purpose::URL_SAFE_NO_PAD
                    .encode("<p>attachment body</p>")
            })),
        );

        let (client, state, server) = spawn_fake(fake).await;
        let batch = client.fetch_new(&connection(None, None)).await.unwrap();
        server.abort();

        assert_eq!(batch.next_history_id.as_deref(), Some("baseline-100"));
        assert!(!batch.history_was_reset);
        assert!(batch.failures.is_empty());
        assert_eq!(batch.emails.len(), 2);
        assert_eq!(batch.emails[0].provider_message_id, "earlier");
        assert_eq!(
            batch.emails[0].body_html.as_deref(),
            Some("<p>attachment body</p>")
        );
        assert_eq!(batch.emails[1].provider_message_id, "later");
        assert_eq!(
            batch.emails[1].body_text.as_deref(),
            Some("inline unpadded body")
        );

        let calls = state.calls.lock().unwrap();
        let list_calls: Vec<_> = calls
            .iter()
            .filter(|call| call.kind == "messages")
            .collect();
        assert_eq!(list_calls.len(), 2);
        assert!(
            list_calls[0]
                .query
                .get("q")
                .unwrap()
                .contains("newer_than:30d")
        );
        assert_eq!(
            list_calls[1].query.get("pageToken").map(String::as_str),
            Some("page-2")
        );
        assert!(
            calls
                .iter()
                .all(|call| { call.authorization.as_deref() == Some("Bearer test-access-token") })
        );
        assert!(
            calls
                .iter()
                .any(|call| { call.kind == "attachment:earlier:body-attachment" })
        );
    }

    #[tokio::test]
    async fn untrusted_metadata_is_a_durable_ignored_outcome_without_body_download() {
        let mut fake = FakeGmail::new("baseline-ignored");
        fake.message_pages.insert(
            String::new(),
            FakeResponse::ok(json!({"messages": [{"id": "spoofed"}]})),
        );
        fake.metadata.insert(
            "spoofed".to_string(),
            FakeResponse::ok(json!({
                "id": "spoofed",
                "payload": {
                    "headers": [
                        {"name": "From", "value": "Netflix <info@account.netflix.com>"},
                        {
                            "name": "Authentication-Results",
                            "value": "mx.google.com; dkim=pass header.d=evil.example"
                        }
                    ]
                }
            })),
        );

        let (client, state, server) = spawn_fake(fake).await;
        let batch = client.fetch_new(&connection(None, None)).await.unwrap();
        server.abort();

        assert!(batch.emails.is_empty());
        assert!(batch.failures.is_empty());
        assert_eq!(batch.ignored_message_ids, vec!["spoofed"]);
        assert_eq!(batch.next_history_id.as_deref(), Some("baseline-ignored"));
        let calls = state.calls.lock().unwrap();
        assert!(
            calls
                .iter()
                .any(|call| call.kind == "message:metadata:spoofed")
        );
        assert!(!calls.iter().any(|call| call.kind == "message:full:spoofed"));
    }

    #[tokio::test]
    async fn incremental_sync_paginates_history_and_deduplicates_message_ids() {
        let mut fake = FakeGmail::new("unused");
        fake.history_pages.insert(
            String::new(),
            FakeResponse::ok(json!({
                "history": [{"messagesAdded": [{"message": {"id": "one"}}]}],
                "historyId": "201",
                "nextPageToken": "history-page-2"
            })),
        );
        fake.history_pages.insert(
            "history-page-2".to_string(),
            FakeResponse::ok(json!({
                "history": [{"messagesAdded": [
                    {"message": {"id": "one"}},
                    {"message": {"id": "two"}}
                ]}],
                "historyId": "202"
            })),
        );
        let body = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode("receipt");
        for (id, millis) in [("one", 1_700_000_001_000), ("two", 1_700_000_002_000)] {
            fake.metadata
                .insert(id.to_string(), FakeResponse::ok(trusted_metadata(id)));
            fake.full_messages.insert(
                id.to_string(),
                FakeResponse::ok(full_text_message(id, millis, &body)),
            );
        }

        let (client, state, server) = spawn_fake(fake).await;
        let batch = client
            .fetch_new(&connection(Some("cursor-200"), None))
            .await
            .unwrap();
        server.abort();

        assert_eq!(batch.next_history_id.as_deref(), Some("202"));
        assert!(!batch.history_was_reset);
        assert_eq!(batch.emails.len(), 2);
        assert!(batch.failures.is_empty());

        let calls = state.calls.lock().unwrap();
        let history_calls: Vec<_> = calls.iter().filter(|call| call.kind == "history").collect();
        assert_eq!(history_calls.len(), 2);
        for call in &history_calls {
            assert_eq!(
                call.query.get("startHistoryId").map(String::as_str),
                Some("cursor-200")
            );
            assert_eq!(
                call.query.get("historyTypes").map(String::as_str),
                Some("messageAdded")
            );
        }
        assert_eq!(
            history_calls[1].query.get("pageToken").map(String::as_str),
            Some("history-page-2")
        );
        assert!(!calls.iter().any(|call| call.kind == "profile"));
        assert!(!calls.iter().any(|call| call.kind == "messages"));
    }

    #[tokio::test]
    async fn expired_history_cursor_captures_new_baseline_and_uses_overlap_scan() {
        let last_sync = DateTime::parse_from_rfc3339("2026-05-20T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let mut fake = FakeGmail::new("replacement-baseline");
        fake.history_pages
            .insert(String::new(), FakeResponse::status(StatusCode::NOT_FOUND));
        fake.message_pages
            .insert(String::new(), FakeResponse::ok(json!({"messages": []})));

        let (client, state, server) = spawn_fake(fake).await;
        let batch = client
            .fetch_new(&connection(Some("expired-cursor"), Some(last_sync)))
            .await
            .unwrap();
        server.abort();

        assert!(batch.history_was_reset);
        assert_eq!(
            batch.next_history_id.as_deref(),
            Some("replacement-baseline")
        );
        assert!(batch.emails.is_empty());

        let calls = state.calls.lock().unwrap();
        let list = calls.iter().find(|call| call.kind == "messages").unwrap();
        let expected_after = (last_sync - Duration::hours(24)).timestamp().to_string();
        assert!(
            list.query
                .get("q")
                .unwrap()
                .contains(&format!("after:{expected_after}"))
        );
        assert!(calls.iter().any(|call| call.kind == "profile"));
    }

    #[tokio::test]
    async fn poison_message_is_reported_without_losing_other_messages_or_cursor() {
        let mut fake = FakeGmail::new("baseline-after-poison");
        fake.message_pages.insert(
            String::new(),
            FakeResponse::ok(json!({"messages": [{"id": "good"}, {"id": "poison"}]})),
        );
        for id in ["good", "poison"] {
            fake.metadata
                .insert(id.to_string(), FakeResponse::ok(trusted_metadata(id)));
        }
        let body = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode("valid body");
        fake.full_messages.insert(
            "good".to_string(),
            FakeResponse::ok(full_text_message("good", 1_700_000_001_000, &body)),
        );
        fake.full_messages.insert(
            "poison".to_string(),
            FakeResponse::ok(full_text_message(
                "poison",
                1_700_000_002_000,
                "***not-base64url***",
            )),
        );

        let (client, _state, server) = spawn_fake(fake).await;
        let batch = client.fetch_new(&connection(None, None)).await.unwrap();
        server.abort();

        assert_eq!(
            batch.next_history_id.as_deref(),
            Some("baseline-after-poison")
        );
        assert_eq!(batch.emails.len(), 1);
        assert_eq!(batch.emails[0].provider_message_id, "good");
        assert_eq!(batch.failures.len(), 1);
        assert_eq!(batch.failures[0].provider_message_id, "poison");
        assert_eq!(batch.failures[0].error_kind, "message_fetch_failed");
    }

    #[tokio::test]
    async fn authentication_rate_limit_and_server_errors_fail_the_connection_sync() {
        for status in [
            StatusCode::UNAUTHORIZED,
            StatusCode::TOO_MANY_REQUESTS,
            StatusCode::INTERNAL_SERVER_ERROR,
            StatusCode::SERVICE_UNAVAILABLE,
        ] {
            let mut fake = FakeGmail::new("baseline-error");
            fake.message_pages.insert(
                String::new(),
                FakeResponse::ok(json!({"messages": [{"id": "blocked"}]})),
            );
            fake.metadata
                .insert("blocked".to_string(), FakeResponse::status(status));

            let (client, _state, server) = spawn_fake(fake).await;
            let error = client
                .fetch_new(&connection(None, None))
                .await
                .expect_err("connection-level response must abort the sync");
            server.abort();

            assert!(
                error
                    .downcast_ref::<reqwest::Error>()
                    .and_then(reqwest::Error::status)
                    .is_some_and(|actual| actual == status),
                "unexpected error for {status}: {error:#}"
            );
        }
    }
}
