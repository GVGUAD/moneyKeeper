//! Gmail wire adapter boundary. Provider response bodies are never logged.

#[derive(Clone)]
pub(crate) struct GmailClient {
    client: reqwest::Client,
    base_url: String,
}

impl GmailClient {
    pub(crate) fn new(base_url: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: base_url.into(),
        }
    }
}

impl crate::contexts::mail::application::ports::GmailSource for GmailClient {
    async fn fetch_page(
        &self,
        access_token: &str,
        cursor: Option<&str>,
    ) -> anyhow::Result<crate::contexts::mail::application::ports::GmailPage> {
        #[derive(serde::Deserialize)]
        struct MessageRef {
            id: String,
        }
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct WirePage {
            #[serde(default)]
            messages: Vec<MessageRef>,
            next_page_token: Option<String>,
        }
        let mut request = self
            .client
            .get(format!("{}/gmail/v1/users/me/messages", self.base_url))
            .bearer_auth(access_token)
            .query(&[("maxResults", "100")]);
        if let Some(cursor) = cursor {
            request = request.query(&[("pageToken", cursor)]);
        }
        let page: WirePage = request.send().await?.error_for_status()?.json().await?;
        let mut messages = Vec::with_capacity(page.messages.len());
        for message in page.messages {
            let wire: WireMessage = self
                .client
                .get(format!(
                    "{}/gmail/v1/users/me/messages/{}",
                    self.base_url, message.id
                ))
                .bearer_auth(access_token)
                .query(&[("format", "full")])
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;
            messages.push(wire.normalize()?);
        }
        Ok(crate::contexts::mail::application::ports::GmailPage {
            messages,
            next_cursor: page.next_page_token,
        })
    }
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireMessage {
    id: String,
    internal_date: String,
    payload: MimePart,
}

#[derive(serde::Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct MimePart {
    #[serde(default)]
    mime_type: String,
    #[serde(default)]
    headers: Vec<Header>,
    #[serde(default)]
    body: MimeBody,
    #[serde(default)]
    parts: Vec<MimePart>,
}

#[derive(serde::Deserialize)]
struct Header {
    name: String,
    value: String,
}

#[derive(serde::Deserialize, Default)]
struct MimeBody {
    data: Option<String>,
}

impl WireMessage {
    fn normalize(self) -> anyhow::Result<crate::contexts::mail::application::ports::GmailMessage> {
        use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
        use chrono::TimeZone as _;
        let from = header(&self.payload.headers, "From").unwrap_or_default();
        let subject = header(&self.payload.headers, "Subject").unwrap_or_default();
        let mut body_text = None;
        let mut body_html = None;
        collect_bodies(
            &self.payload,
            &mut body_text,
            &mut body_html,
            &URL_SAFE_NO_PAD,
        )?;
        let millis = self.internal_date.parse::<i64>()?;
        let received_at = chrono::Utc
            .timestamp_millis_opt(millis)
            .single()
            .ok_or_else(|| anyhow::anyhow!("Gmail message time is outside the supported range"))?;
        Ok(crate::contexts::mail::application::ports::GmailMessage {
            provider_id: self.id,
            from,
            subject,
            body_text,
            body_html,
            received_at,
        })
    }
}

fn header(headers: &[Header], name: &str) -> Option<String> {
    headers
        .iter()
        .find(|header| header.name.eq_ignore_ascii_case(name))
        .map(|header| header.value.clone())
}

fn collect_bodies(
    part: &MimePart,
    text: &mut Option<String>,
    html: &mut Option<String>,
    decoder: &base64::engine::general_purpose::GeneralPurpose,
) -> anyhow::Result<()> {
    use base64::Engine as _;
    if let Some(data) = part.body.data.as_deref() {
        let decoded = String::from_utf8(decoder.decode(data)?)?;
        match part.mime_type.as_str() {
            "text/plain" if text.is_none() => *text = Some(decoded),
            "text/html" if html.is_none() => *html = Some(decoded),
            _ => {}
        }
    }
    for child in &part.parts {
        collect_bodies(child, text, html, decoder)?;
    }
    Ok(())
}
