use async_trait::async_trait;
use base64::Engine;
use chrono::{DateTime, Utc};
use serde::Deserialize;

use crate::domain::email::{EmailFetcher, RawEmail};
use crate::domain::email_connection::EmailConnection;

const SENDER_QUERY: &str = "from:(googleplay-noreply@google.com OR info@account.netflix.com OR no_reply@email.apple.com) newer_than:30d";

pub struct GmailClient {
    http: reqwest::Client,
    client_id: String,
    client_secret: String,
}

impl GmailClient {
    pub fn new(client_id: String, client_secret: String) -> Self {
        Self {
            http: reqwest::Client::new(),
            client_id,
            client_secret,
        }
    }

    async fn ensure_access_token(&self, conn: &EmailConnection) -> anyhow::Result<String> {
        if conn.access_token_expires_at > Utc::now() + chrono::Duration::seconds(30) {
            return Ok(conn.oauth_access_token.clone());
        }
        let refreshed = crate::infrastructure::email::oauth::refresh_gmail_token(
            &self.http,
            &self.client_id,
            &self.client_secret,
            &conn.oauth_refresh_token,
        )
        .await?;
        Ok(refreshed.access_token)
    }
}

#[derive(Deserialize)]
struct MessagesListResponse {
    messages: Option<Vec<MessageRef>>,
    #[serde(rename = "nextPageToken")]
    next_page_token: Option<String>,
}

#[derive(Deserialize)]
struct MessageRef {
    id: String,
}

#[derive(Deserialize)]
struct MessageFull {
    id: String,
    payload: Payload,
    #[serde(rename = "internalDate")]
    internal_date: String,
}

#[derive(Deserialize)]
struct Payload {
    headers: Vec<Header>,
    body: Option<Body>,
    parts: Option<Vec<Payload>>,
    #[serde(rename = "mimeType")]
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
    size: Option<i64>,
}

fn header(payload: &Payload, name: &str) -> Option<String> {
    payload
        .headers
        .iter()
        .find(|h| h.name.eq_ignore_ascii_case(name))
        .map(|h| h.value.clone())
}

fn decode_body(body: &Body) -> Option<String> {
    let raw = body.data.as_ref()?;
    let bytes = base64::engine::general_purpose::URL_SAFE.decode(raw).ok()?;
    String::from_utf8(bytes).ok()
}

fn collect_bodies(payload: &Payload, text: &mut Option<String>, html: &mut Option<String>) {
    if let Some(body) = payload.body.as_ref()
        && body.size.unwrap_or(0) > 0
    {
        if payload.mime_type == "text/plain" && text.is_none() {
            *text = decode_body(body);
        } else if payload.mime_type == "text/html" && html.is_none() {
            *html = decode_body(body);
        }
    }
    if let Some(parts) = payload.parts.as_ref() {
        for p in parts {
            collect_bodies(p, text, html);
        }
    }
}

fn payload_to_raw(msg: MessageFull) -> anyhow::Result<RawEmail> {
    let from = header(&msg.payload, "From").unwrap_or_default();
    let subject = header(&msg.payload, "Subject").unwrap_or_default();
    let message_id = header(&msg.payload, "Message-ID")
        .or_else(|| header(&msg.payload, "Message-Id"))
        .unwrap_or_else(|| msg.id.clone());
    let received_at = msg
        .internal_date
        .parse::<i64>()
        .ok()
        .and_then(|ms| DateTime::<Utc>::from_timestamp(ms / 1000, 0))
        .unwrap_or_else(Utc::now);
    let mut text = None;
    let mut html = None;
    collect_bodies(&msg.payload, &mut text, &mut html);
    Ok(RawEmail {
        message_id,
        from,
        subject,
        received_at,
        body_text: text,
        body_html: html,
    })
}

#[async_trait]
impl EmailFetcher for GmailClient {
    async fn fetch_new(
        &self,
        conn: &EmailConnection,
    ) -> anyhow::Result<(Vec<RawEmail>, Option<String>)> {
        let access_token = self.ensure_access_token(conn).await?;

        let mut message_ids: Vec<String> = Vec::new();
        let mut page_token: Option<String> = None;

        loop {
            let mut url = format!(
                "https://gmail.googleapis.com/gmail/v1/users/me/messages?q={}",
                urlencoding::encode(SENDER_QUERY)
            );
            if let Some(tok) = &page_token {
                url.push_str(&format!("&pageToken={tok}"));
            }
            let resp: MessagesListResponse = self
                .http
                .get(&url)
                .bearer_auth(&access_token)
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;
            if let Some(msgs) = resp.messages {
                message_ids.extend(msgs.into_iter().map(|m| m.id));
            }
            match resp.next_page_token {
                Some(t) => page_token = Some(t),
                None => break,
            }
        }

        let mut out = Vec::new();
        for id in message_ids {
            let url =
                format!("https://gmail.googleapis.com/gmail/v1/users/me/messages/{id}?format=full");
            let msg: MessageFull = self
                .http
                .get(&url)
                .bearer_auth(&access_token)
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;
            out.push(payload_to_raw(msg)?);
        }

        Ok((out, conn.last_history_id.clone()))
    }
}
