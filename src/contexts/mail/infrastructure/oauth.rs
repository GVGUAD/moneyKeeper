//! Gmail OAuth adapter. Provider response bodies and credentials are never logged.

use async_trait::async_trait;
use chrono::{Duration, Utc};
use serde::Deserialize;
use std::time::Instant;

use crate::contexts::mail::application::ports::{GmailOAuth, OAuthTokens};

const GOOGLE_AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const GOOGLE_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const GMAIL_SCOPES: &str =
    "https://www.googleapis.com/auth/gmail.readonly https://www.googleapis.com/auth/userinfo.email";

#[derive(Debug, thiserror::Error)]
pub(crate) enum OAuthProviderError {
    #[error("Gmail OAuth credentials are invalid or revoked")]
    InvalidCredentials,
    #[error("Gmail OAuth provider is temporarily unavailable")]
    Transient,
    #[error("Gmail OAuth provider rejected the request")]
    Rejected,
    #[error("Gmail OAuth is not configured")]
    Configuration,
}

#[derive(Clone)]
pub(crate) struct GoogleOAuthClient {
    http: reqwest::Client,
    client_id: String,
    client_secret: String,
    redirect_uri: String,
}

impl std::fmt::Debug for GoogleOAuthClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GoogleOAuthClient")
            .field("client_id", &"[REDACTED]")
            .field("client_secret", &"[REDACTED]")
            .field("redirect_uri", &"[REDACTED]")
            .finish_non_exhaustive()
    }
}

impl GoogleOAuthClient {
    pub(crate) fn from_environment() -> Self {
        Self::new(
            std::env::var("GMAIL_CLIENT_ID").unwrap_or_default(),
            std::env::var("GMAIL_CLIENT_SECRET").unwrap_or_default(),
            std::env::var("GMAIL_REDIRECT_URI")
                .unwrap_or_else(|_| "http://localhost:8080/oauth/gmail/callback".to_owned()),
        )
    }

    pub(crate) fn new(
        client_id: impl Into<String>,
        client_secret: impl Into<String>,
        redirect_uri: impl Into<String>,
    ) -> Self {
        Self {
            http: reqwest::Client::builder()
                .connect_timeout(std::time::Duration::from_secs(10))
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .expect("static Gmail OAuth HTTP configuration is valid"),
            client_id: client_id.into(),
            client_secret: client_secret.into(),
            redirect_uri: redirect_uri.into(),
        }
    }

    fn configured(&self) -> anyhow::Result<()> {
        if self.client_id.trim().is_empty() || self.client_secret.trim().is_empty() {
            return Err(OAuthProviderError::Configuration.into());
        }
        Ok(())
    }

    async fn token_request(
        &self,
        operation: &'static str,
        fields: &[(&str, &str)],
    ) -> anyhow::Result<OAuthTokens> {
        #[derive(Deserialize)]
        struct TokenResponse {
            access_token: String,
            #[serde(default)]
            refresh_token: Option<String>,
            expires_in: i64,
        }

        self.configured()?;
        let started = Instant::now();
        let response = self
            .http
            .post(GOOGLE_TOKEN_URL)
            .form(fields)
            .send()
            .await
            .map_err(|error| {
                log_oauth_transport(operation, &error, started.elapsed());
                OAuthProviderError::Transient
            })?;
        if !response.status().is_success() {
            #[derive(Deserialize)]
            struct ErrorResponse {
                #[serde(default)]
                error: String,
            }
            let status = response.status();
            let error_code = response
                .json::<ErrorResponse>()
                .await
                .map(|body| body.error)
                .unwrap_or_default();
            if error_code == "invalid_grant" || status == reqwest::StatusCode::UNAUTHORIZED {
                log_oauth_failure(operation, status, "invalid_credentials", started.elapsed());
                return Err(OAuthProviderError::InvalidCredentials.into());
            }
            if status == reqwest::StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
                log_oauth_failure(operation, status, "transient", started.elapsed());
                return Err(OAuthProviderError::Transient.into());
            }
            log_oauth_failure(operation, status, "rejected", started.elapsed());
            return Err(OAuthProviderError::Rejected.into());
        }
        let status = response.status();
        let response = response.json::<TokenResponse>().await.map_err(|_| {
            log_oauth_failure(operation, status, "invalid_response", started.elapsed());
            OAuthProviderError::Rejected
        })?;
        if response.access_token.trim().is_empty() || response.expires_in <= 0 {
            log_oauth_failure(operation, status, "invalid_response", started.elapsed());
            return Err(OAuthProviderError::Rejected.into());
        }
        tracing::info!(
            event.name = "provider.request.completed",
            provider = "google_oauth",
            operation,
            outcome = "success",
            http.status = status.as_u16(),
            duration_ms = elapsed_ms(started.elapsed()),
            "Provider request completed"
        );
        Ok(OAuthTokens {
            access_token: response.access_token,
            refresh_token: response
                .refresh_token
                .filter(|value| !value.trim().is_empty()),
            expires_at: Utc::now() + Duration::seconds((response.expires_in - 60).max(1)),
        })
    }
}

fn log_oauth_transport(
    operation: &'static str,
    error: &reqwest::Error,
    elapsed: std::time::Duration,
) {
    tracing::warn!(
        event.name = "provider.request.completed",
        provider = "google_oauth",
        operation,
        outcome = transport_outcome(error),
        duration_ms = elapsed_ms(elapsed),
        "Provider request completed"
    );
}

fn log_oauth_failure(
    operation: &'static str,
    status: reqwest::StatusCode,
    outcome: &'static str,
    elapsed: std::time::Duration,
) {
    tracing::warn!(
        event.name = "provider.request.completed",
        provider = "google_oauth",
        operation,
        outcome,
        http.status = status.as_u16(),
        duration_ms = elapsed_ms(elapsed),
        "Provider request completed"
    );
}

fn transport_outcome(error: &reqwest::Error) -> &'static str {
    if error.is_timeout() {
        "timeout"
    } else if error.is_connect() {
        "connect_error"
    } else {
        "transport_error"
    }
}

fn elapsed_ms(elapsed: std::time::Duration) -> u64 {
    u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
}

#[async_trait]
impl GmailOAuth for GoogleOAuthClient {
    fn authorization_url(&self, state: &str, challenge: &str) -> anyhow::Result<String> {
        self.configured()?;
        let mut url = reqwest::Url::parse(GOOGLE_AUTH_URL)?;
        url.query_pairs_mut()
            .append_pair("client_id", &self.client_id)
            .append_pair("redirect_uri", &self.redirect_uri)
            .append_pair("response_type", "code")
            .append_pair("scope", GMAIL_SCOPES)
            .append_pair("access_type", "offline")
            .append_pair("prompt", "consent")
            .append_pair("state", state)
            .append_pair("code_challenge", challenge)
            .append_pair("code_challenge_method", "S256");
        Ok(url.into())
    }

    async fn exchange(&self, code: &str, verifier: &str) -> anyhow::Result<OAuthTokens> {
        self.token_request(
            "exchange_token",
            &[
                ("client_id", &self.client_id),
                ("client_secret", &self.client_secret),
                ("code", code),
                ("code_verifier", verifier),
                ("grant_type", "authorization_code"),
                ("redirect_uri", &self.redirect_uri),
            ],
        )
        .await
    }

    async fn refresh(&self, refresh_token: &str) -> anyhow::Result<OAuthTokens> {
        let mut tokens = self
            .token_request(
                "refresh_token",
                &[
                    ("client_id", &self.client_id),
                    ("client_secret", &self.client_secret),
                    ("refresh_token", refresh_token),
                    ("grant_type", "refresh_token"),
                ],
            )
            .await?;
        tokens.refresh_token = Some(refresh_token.to_owned());
        Ok(tokens)
    }
}

/// OAuth state values are opaque and redacted when formatted.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct OAuthState(String);

impl std::fmt::Debug for OAuthState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("OAuthState([REDACTED])")
    }
}

impl OAuthState {
    pub(crate) fn new(value: String) -> Option<Self> {
        (!value.is_empty()).then_some(Self(value))
    }

    pub(crate) fn expose_for_redirect(&self) -> &str {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authorization_url_contains_pkce_and_never_exposes_the_client_secret() {
        let client = GoogleOAuthClient::new(
            "client-id",
            "top-secret",
            "https://example.test/oauth/gmail/callback",
        );
        let url = client
            .authorization_url("opaque-state", "pkce-challenge")
            .unwrap();
        assert!(url.contains("client_id=client-id"));
        assert!(url.contains("state=opaque-state"));
        assert!(url.contains("code_challenge=pkce-challenge"));
        assert!(url.contains("access_type=offline"));
        assert!(!url.contains("top-secret"));
        let debug = format!("{client:?}");
        assert!(!debug.contains("top-secret"));
        assert!(!debug.contains("client-id"));
        assert!(!debug.contains("example.test"));
    }

    #[test]
    fn token_debug_output_is_redacted() {
        let tokens = OAuthTokens {
            access_token: "access-secret".to_owned(),
            refresh_token: Some("refresh-secret".to_owned()),
            expires_at: Utc::now(),
        };
        let debug = format!("{tokens:?}");
        assert!(!debug.contains("access-secret"));
        assert!(!debug.contains("refresh-secret"));
    }
}
