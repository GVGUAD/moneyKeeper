use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use base64::Engine;
use chrono::{DateTime, Duration, Utc};
use rand::RngCore;
use rand::rngs::OsRng;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use uuid::Uuid;

use crate::domain::email_connection::{
    EmailConnection, EmailConnectionRepository, EmailConnectionStatus, EmailProvider,
};
use crate::domain::subscription_error::SubscriptionError;
use crate::infrastructure::credential_crypto::{SecretValue, TokenCipher, oauth_pkce_aad};

const DEFAULT_STATE_TTL_MINUTES: i64 = 10;
const GMAIL_SCOPES: &str =
    "https://www.googleapis.com/auth/gmail.readonly https://www.googleapis.com/auth/userinfo.email";

#[derive(Clone)]
pub struct OAuthConfig {
    pub client_id: String,
    pub client_secret: SecretValue,
    pub redirect_uri: String,
    pub success_redirect_uri: Option<String>,
    pub failure_redirect_uri: Option<String>,
    pub state_ttl: Duration,
}

impl OAuthConfig {
    pub fn new(client_id: String, client_secret: impl Into<String>, redirect_uri: String) -> Self {
        Self {
            client_id,
            client_secret: SecretValue::new(client_secret),
            redirect_uri,
            success_redirect_uri: None,
            failure_redirect_uri: None,
            state_ttl: Duration::minutes(DEFAULT_STATE_TTL_MINUTES),
        }
    }

    pub fn with_result_redirects(
        mut self,
        success_redirect_uri: Option<String>,
        failure_redirect_uri: Option<String>,
    ) -> Self {
        self.success_redirect_uri = success_redirect_uri;
        self.failure_redirect_uri = failure_redirect_uri;
        self
    }
}

impl fmt::Debug for OAuthConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OAuthConfig")
            .field("client_id", &self.client_id)
            .field("client_secret", &"[REDACTED]")
            .field("redirect_uri", &self.redirect_uri)
            .field("success_redirect_uri", &self.success_redirect_uri)
            .field("failure_redirect_uri", &self.failure_redirect_uri)
            .field("state_ttl", &self.state_ttl)
            .finish()
    }
}

pub struct GmailTokenSet {
    pub access_token: SecretValue,
    pub refresh_token: Option<SecretValue>,
    pub expires_at: DateTime<Utc>,
}

impl fmt::Debug for GmailTokenSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GmailTokenSet")
            .field("access_token", &"[REDACTED]")
            .field(
                "refresh_token",
                &self.refresh_token.as_ref().map(|_| "[REDACTED]"),
            )
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GmailProfile {
    pub email_address: String,
    pub verified: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum GmailProviderError {
    #[error("Gmail credentials are invalid or revoked")]
    InvalidCredentials,
    #[error("Gmail provider is temporarily unavailable")]
    Transient,
    #[error("Gmail provider rejected the request")]
    Rejected,
}

#[async_trait]
pub trait GmailOAuthClient: Send + Sync {
    fn authorization_url(&self, state: &str, pkce_challenge: &str) -> anyhow::Result<String>;
    async fn exchange_code(&self, code: &str, pkce_verifier: &str)
    -> anyhow::Result<GmailTokenSet>;
    async fn profile(&self, access_token: &str) -> anyhow::Result<GmailProfile>;
    async fn refresh(&self, refresh_token: &str) -> anyhow::Result<GmailTokenSet>;
    async fn revoke(&self, token: &str) -> anyhow::Result<()>;
}

pub struct ReqwestGmailOAuthClient {
    http: reqwest::Client,
    config: OAuthConfig,
}

impl ReqwestGmailOAuthClient {
    pub fn new(config: OAuthConfig) -> Self {
        Self {
            http: reqwest::Client::builder()
                .connect_timeout(std::time::Duration::from_secs(10))
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .expect("valid Gmail OAuth HTTP client configuration"),
            config,
        }
    }

    pub fn with_http(config: OAuthConfig, http: reqwest::Client) -> Self {
        Self { http, config }
    }

    async fn token_request(&self, fields: &[(&str, &str)]) -> anyhow::Result<GmailTokenSet> {
        #[derive(Deserialize)]
        struct TokenResponse {
            access_token: String,
            #[serde(default)]
            refresh_token: Option<String>,
            expires_in: i64,
        }

        let response = self
            .http
            .post("https://oauth2.googleapis.com/token")
            .form(fields)
            .send()
            .await
            .map_err(|_| GmailProviderError::Transient)?;
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
                return Err(GmailProviderError::InvalidCredentials.into());
            }
            if status == reqwest::StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
                return Err(GmailProviderError::Transient.into());
            }
            return Err(GmailProviderError::Rejected.into());
        }
        let response = response
            .json::<TokenResponse>()
            .await
            .map_err(|_| GmailProviderError::Rejected)?;
        if response.access_token.trim().is_empty() || response.expires_in <= 0 {
            return Err(GmailProviderError::Rejected.into());
        }
        Ok(GmailTokenSet {
            access_token: SecretValue::new(response.access_token),
            refresh_token: response.refresh_token.map(SecretValue::new),
            expires_at: Utc::now() + Duration::seconds((response.expires_in - 60).max(1)),
        })
    }
}

impl fmt::Debug for ReqwestGmailOAuthClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ReqwestGmailOAuthClient")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl GmailOAuthClient for ReqwestGmailOAuthClient {
    fn authorization_url(&self, state: &str, pkce_challenge: &str) -> anyhow::Result<String> {
        let mut url = reqwest::Url::parse("https://accounts.google.com/o/oauth2/v2/auth")?;
        url.query_pairs_mut()
            .append_pair("client_id", &self.config.client_id)
            .append_pair("redirect_uri", &self.config.redirect_uri)
            .append_pair("response_type", "code")
            .append_pair("scope", GMAIL_SCOPES)
            .append_pair("access_type", "offline")
            .append_pair("prompt", "consent")
            .append_pair("state", state)
            .append_pair("code_challenge", pkce_challenge)
            .append_pair("code_challenge_method", "S256");
        Ok(url.into())
    }

    async fn exchange_code(
        &self,
        code: &str,
        pkce_verifier: &str,
    ) -> anyhow::Result<GmailTokenSet> {
        self.token_request(&[
            ("client_id", &self.config.client_id),
            ("client_secret", self.config.client_secret.expose()),
            ("code", code),
            ("code_verifier", pkce_verifier),
            ("grant_type", "authorization_code"),
            ("redirect_uri", &self.config.redirect_uri),
        ])
        .await
    }

    async fn profile(&self, access_token: &str) -> anyhow::Result<GmailProfile> {
        #[derive(Deserialize)]
        struct ProfileResponse {
            email: String,
            #[serde(default)]
            verified_email: bool,
        }
        let response = self
            .http
            .get("https://www.googleapis.com/oauth2/v2/userinfo")
            .bearer_auth(access_token)
            .send()
            .await
            .map_err(|_| GmailProviderError::Transient)?;
        let status = response.status();
        if !status.is_success() {
            if status == reqwest::StatusCode::UNAUTHORIZED
                || status == reqwest::StatusCode::FORBIDDEN
            {
                return Err(GmailProviderError::InvalidCredentials.into());
            }
            if status == reqwest::StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
                return Err(GmailProviderError::Transient.into());
            }
            return Err(GmailProviderError::Rejected.into());
        }
        let profile = response
            .json::<ProfileResponse>()
            .await
            .map_err(|_| GmailProviderError::Rejected)?;
        let email_address =
            normalize_gmail_address(&profile.email).map_err(|_| GmailProviderError::Rejected)?;
        Ok(GmailProfile {
            email_address,
            verified: profile.verified_email,
        })
    }

    async fn refresh(&self, refresh_token: &str) -> anyhow::Result<GmailTokenSet> {
        self.token_request(&[
            ("client_id", &self.config.client_id),
            ("client_secret", self.config.client_secret.expose()),
            ("refresh_token", refresh_token),
            ("grant_type", "refresh_token"),
        ])
        .await
    }

    async fn revoke(&self, token: &str) -> anyhow::Result<()> {
        let response = self
            .http
            .post("https://oauth2.googleapis.com/revoke")
            .form(&[("token", token)])
            .send()
            .await
            .map_err(|error| anyhow::anyhow!("Gmail token revocation request failed: {error}"))?;
        if response.status().is_success() || response.status() == reqwest::StatusCode::BAD_REQUEST {
            return Ok(());
        }
        Err(anyhow::anyhow!(
            "Gmail token revocation rejected with status {}",
            response.status()
        ))
    }
}

pub struct PendingOAuthState {
    pub user_id: Uuid,
    pub pkce_verifier: SecretValue,
    pub expires_at: DateTime<Utc>,
}

impl fmt::Debug for PendingOAuthState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PendingOAuthState")
            .field("user_id", &self.user_id)
            .field("pkce_verifier", &"[REDACTED]")
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

#[async_trait]
pub trait GmailOAuthStateStore: Send + Sync {
    async fn create(&self, state_hash: &[u8], state: &PendingOAuthState) -> anyhow::Result<()>;
    /// Atomically consumes a valid state. `expected_user_id` is supplied for
    /// authenticated POST callbacks; public browser callbacks use the state as
    /// their one-time authority and pass `None`.
    async fn consume(
        &self,
        state_hash: &[u8],
        expected_user_id: Option<Uuid>,
    ) -> anyhow::Result<Option<PendingOAuthState>>;
    async fn purge_expired(&self) -> anyhow::Result<u64>;
}

pub struct PgGmailOAuthStateStore {
    pool: PgPool,
    cipher: Arc<TokenCipher>,
}

impl PgGmailOAuthStateStore {
    pub fn new(pool: PgPool, cipher: Arc<TokenCipher>) -> Self {
        Self { pool, cipher }
    }
}

#[async_trait]
impl GmailOAuthStateStore for PgGmailOAuthStateStore {
    async fn create(&self, state_hash: &[u8], state: &PendingOAuthState) -> anyhow::Result<()> {
        let encrypted = self
            .cipher
            .encrypt(state.pkce_verifier.expose(), &oauth_pkce_aad(state_hash))?;
        sqlx::query(
            "INSERT INTO gmail_oauth_states \
             (state_hash, user_id, pkce_verifier_encrypted, expires_at) VALUES ($1,$2,$3,$4)",
        )
        .bind(state_hash)
        .bind(state.user_id)
        .bind(encrypted)
        .bind(state.expires_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn consume(
        &self,
        state_hash: &[u8],
        expected_user_id: Option<Uuid>,
    ) -> anyhow::Result<Option<PendingOAuthState>> {
        #[derive(sqlx::FromRow)]
        struct Row {
            user_id: Uuid,
            pkce_verifier_encrypted: String,
            expires_at: DateTime<Utc>,
        }
        let row = sqlx::query_as::<_, Row>(
            "UPDATE gmail_oauth_states SET consumed_at=now() \
             WHERE state_hash=$1 AND consumed_at IS NULL AND expires_at>now() \
               AND ($2::uuid IS NULL OR user_id=$2) \
             RETURNING user_id, pkce_verifier_encrypted, expires_at",
        )
        .bind(state_hash)
        .bind(expected_user_id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|row| {
            Ok(PendingOAuthState {
                user_id: row.user_id,
                pkce_verifier: self
                    .cipher
                    .decrypt(&row.pkce_verifier_encrypted, &oauth_pkce_aad(state_hash))?,
                expires_at: row.expires_at,
            })
        })
        .transpose()
    }

    async fn purge_expired(&self) -> anyhow::Result<u64> {
        let result = sqlx::query(
            "DELETE FROM gmail_oauth_states WHERE expires_at<=now() OR consumed_at IS NOT NULL",
        )
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum OAuthFlowError {
    #[error("OAuth state is invalid, expired, already used, or belongs to another user")]
    InvalidState,
    #[error("OAuth provider did not return a refresh token")]
    MissingRefreshToken,
    #[error("OAuth provider returned incomplete or unverified credentials")]
    InvalidProviderCredentials,
    #[error("OAuth request is incomplete")]
    IncompleteRequest,
}

#[derive(Debug, Clone)]
pub struct GmailOAuthStart {
    pub authorize_url: String,
    pub state: String,
}

pub struct GmailOAuthService {
    client: Arc<dyn GmailOAuthClient>,
    states: Arc<dyn GmailOAuthStateStore>,
    connections: Arc<dyn EmailConnectionRepository>,
    config: OAuthConfig,
}

impl GmailOAuthService {
    pub fn new(
        client: Arc<dyn GmailOAuthClient>,
        states: Arc<dyn GmailOAuthStateStore>,
        connections: Arc<dyn EmailConnectionRepository>,
        config: OAuthConfig,
    ) -> Self {
        Self {
            client,
            states,
            connections,
            config,
        }
    }

    pub fn client(&self) -> &Arc<dyn GmailOAuthClient> {
        &self.client
    }

    pub async fn start(&self, user_id: Uuid) -> anyhow::Result<GmailOAuthStart> {
        self.states.purge_expired().await?;
        let state = random_urlsafe_secret();
        let verifier = random_urlsafe_secret();
        let state_hash = Sha256::digest(state.as_bytes());
        let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(Sha256::digest(verifier.as_bytes()));
        self.states
            .create(
                &state_hash,
                &PendingOAuthState {
                    user_id,
                    pkce_verifier: SecretValue::new(verifier),
                    expires_at: Utc::now() + self.config.state_ttl,
                },
            )
            .await?;
        let authorize_url = self.client.authorization_url(&state, &challenge)?;
        Ok(GmailOAuthStart {
            authorize_url,
            state,
        })
    }

    pub async fn complete(
        &self,
        code: &str,
        state: &str,
        expected_user_id: Option<Uuid>,
    ) -> anyhow::Result<EmailConnection> {
        if code.trim().is_empty() || state.trim().is_empty() {
            return Err(OAuthFlowError::IncompleteRequest.into());
        }
        let state_hash = Sha256::digest(state.as_bytes());
        let pending = self
            .states
            .consume(&state_hash, expected_user_id)
            .await?
            .ok_or(OAuthFlowError::InvalidState)?;
        let mut tokens = self
            .client
            .exchange_code(code, pending.pkce_verifier.expose())
            .await?;
        if tokens.access_token.trim().is_empty() {
            return Err(OAuthFlowError::InvalidProviderCredentials.into());
        }
        let profile = self.client.profile(tokens.access_token.expose()).await?;
        if !profile.verified {
            return Err(OAuthFlowError::InvalidProviderCredentials.into());
        }
        let email_address = normalize_gmail_address(&profile.email_address)
            .map_err(|_| OAuthFlowError::InvalidProviderCredentials)?;
        let existing = self
            .connections
            .list_by_user(pending.user_id)
            .await?
            .into_iter()
            .find(|connection| {
                connection.provider == EmailProvider::Gmail
                    && connection
                        .email_address
                        .eq_ignore_ascii_case(&email_address)
            });
        let refresh_token = match tokens
            .refresh_token
            .take()
            .filter(|token| !token.trim().is_empty())
        {
            Some(token) => token,
            None => existing
                .as_ref()
                .map(|connection| connection.oauth_refresh_token.clone())
                .filter(|token| !token.trim().is_empty())
                .ok_or(OAuthFlowError::MissingRefreshToken)?,
        };
        let now = Utc::now();
        let connection = EmailConnection {
            id: existing
                .as_ref()
                .map_or_else(Uuid::new_v4, |value| value.id),
            user_id: pending.user_id,
            provider: EmailProvider::Gmail,
            email_address,
            oauth_access_token: tokens.access_token,
            oauth_refresh_token: refresh_token,
            credential_version: existing
                .as_ref()
                .map_or(0, |connection| connection.credential_version),
            access_token_expires_at: tokens.expires_at,
            status: EmailConnectionStatus::Connected,
            last_synced_at: None,
            last_history_id: None,
            created_at: now,
        };
        self.connections.upsert_by_address(&connection).await
    }

    /// Consume state for a provider-denied browser callback. Denials carry no
    /// authorization code, but they still must prove and invalidate the same
    /// one-time state to prevent forged callback results and later replay.
    pub async fn consume_denied_state(&self, state: &str) -> anyhow::Result<()> {
        if state.trim().is_empty() {
            return Err(OAuthFlowError::IncompleteRequest.into());
        }
        let state_hash = Sha256::digest(state.as_bytes());
        self.states
            .consume(&state_hash, None)
            .await?
            .ok_or(OAuthFlowError::InvalidState)?;
        Ok(())
    }

    pub async fn disconnect(&self, id: Uuid, user_id: Uuid) -> anyhow::Result<()> {
        let connection = self
            .connections
            .find_by_id(id, user_id)
            .await?
            .ok_or(SubscriptionError::ConnectionNotFound)?;
        self.client.revoke(&connection.oauth_refresh_token).await?;
        self.connections.delete(id, user_id).await
    }

    pub fn success_redirect(&self, connection_id: Uuid) -> Option<String> {
        add_redirect_query(
            self.config.success_redirect_uri.as_deref()?,
            &[
                ("status", "connected"),
                ("connection_id", &connection_id.to_string()),
            ],
        )
        .ok()
    }

    pub fn failure_redirect(&self, code: &str) -> Option<String> {
        add_redirect_query(
            self.config.failure_redirect_uri.as_deref()?,
            &[("status", "error"), ("error", code)],
        )
        .ok()
    }
}

fn random_urlsafe_secret() -> String {
    let mut value = [0_u8; 32];
    OsRng.fill_bytes(&mut value);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(value)
}

fn normalize_gmail_address(value: &str) -> anyhow::Result<String> {
    let normalized = value.trim().to_ascii_lowercase();
    let Some((local, domain)) = normalized.split_once('@') else {
        anyhow::bail!("Gmail profile returned an invalid email address");
    };
    if local.is_empty() || domain.is_empty() || domain.contains('@') {
        anyhow::bail!("Gmail profile returned an invalid email address");
    }
    Ok(normalized)
}

fn add_redirect_query(base: &str, values: &[(&str, &str)]) -> anyhow::Result<String> {
    let mut url = reqwest::Url::parse(base)?;
    url.query_pairs_mut().extend_pairs(values.iter().copied());
    Ok(url.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::email_connection::EmailConnectionRepository;
    use crate::infrastructure::credential_crypto::TokenCipherConfig;
    use crate::infrastructure::email_connection_repository::PgEmailConnectionRepository;
    use crate::infrastructure::test_db;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};

    struct MockOAuthClient {
        last_verifier: Mutex<Option<String>>,
        revoked: AtomicBool,
        include_refresh_token: bool,
        verified_email: bool,
    }

    impl MockOAuthClient {
        fn new(include_refresh_token: bool) -> Self {
            Self {
                last_verifier: Mutex::new(None),
                revoked: AtomicBool::new(false),
                include_refresh_token,
                verified_email: true,
            }
        }

        fn with_unverified_email(mut self) -> Self {
            self.verified_email = false;
            self
        }
    }

    #[async_trait]
    impl GmailOAuthClient for MockOAuthClient {
        fn authorization_url(&self, state: &str, challenge: &str) -> anyhow::Result<String> {
            Ok(format!(
                "https://accounts.example/authorize?state={state}&challenge={challenge}"
            ))
        }

        async fn exchange_code(
            &self,
            code: &str,
            pkce_verifier: &str,
        ) -> anyhow::Result<GmailTokenSet> {
            if code != "valid-code" {
                anyhow::bail!("invalid test code");
            }
            *self.last_verifier.lock().unwrap() = Some(pkce_verifier.to_string());
            Ok(GmailTokenSet {
                access_token: SecretValue::new("access-token"),
                refresh_token: self
                    .include_refresh_token
                    .then(|| SecretValue::new("refresh-token")),
                expires_at: Utc::now() + Duration::hours(1),
            })
        }

        async fn profile(&self, _access_token: &str) -> anyhow::Result<GmailProfile> {
            Ok(GmailProfile {
                email_address: " Alice@Example.COM ".to_string(),
                verified: self.verified_email,
            })
        }

        async fn refresh(&self, _refresh_token: &str) -> anyhow::Result<GmailTokenSet> {
            Ok(GmailTokenSet {
                access_token: SecretValue::new("refreshed-access"),
                refresh_token: None,
                expires_at: Utc::now() + Duration::hours(1),
            })
        }

        async fn revoke(&self, _token: &str) -> anyhow::Result<()> {
            self.revoked.store(true, Ordering::SeqCst);
            Ok(())
        }
    }

    fn test_cipher() -> Arc<TokenCipher> {
        Arc::new(TokenCipher::new(TokenCipherConfig {
            active_key_id: "oauth-test".to_string(),
            keys: [("oauth-test".to_string(), [31_u8; 32])]
                .into_iter()
                .collect(),
        }))
    }

    fn test_config() -> OAuthConfig {
        OAuthConfig::new(
            "client".to_string(),
            "super-secret",
            "https://api.example/oauth/callback".to_string(),
        )
        .with_result_redirects(
            Some("https://app.example/settings".to_string()),
            Some("https://app.example/settings".to_string()),
        )
    }

    #[test]
    fn authorize_url_contains_state_and_pkce_without_secret() {
        let config = OAuthConfig::new(
            "client".to_string(),
            "super-secret",
            "https://app.example/callback".to_string(),
        );
        let client = ReqwestGmailOAuthClient::new(config.clone());
        let url = client
            .authorization_url("state-value", "challenge-value")
            .unwrap();
        let url = reqwest::Url::parse(&url).unwrap();
        let params: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(params.get("state").map(String::as_str), Some("state-value"));
        assert_eq!(
            params.get("code_challenge").map(String::as_str),
            Some("challenge-value")
        );
        assert_eq!(
            params.get("code_challenge_method").map(String::as_str),
            Some("S256")
        );
        assert!(!url.as_str().contains("super-secret"));
        assert!(!format!("{config:?}").contains("super-secret"));
    }

    #[test]
    fn address_normalization_is_strict() {
        assert_eq!(
            normalize_gmail_address(" Alice@Example.COM ").unwrap(),
            "alice@example.com"
        );
        assert!(normalize_gmail_address("not-an-email").is_err());
        assert!(normalize_gmail_address("a@b@c").is_err());
    }

    #[test]
    fn redirect_targets_are_configured_not_user_supplied() {
        let output = add_redirect_query(
            "https://app.example/settings?tab=email",
            &[("status", "connected")],
        )
        .unwrap();
        assert_eq!(
            output,
            "https://app.example/settings?tab=email&status=connected"
        );
    }

    #[tokio::test]
    async fn state_is_user_bound_one_time_pkce_and_connection_is_upserted() {
        let pool = test_db::fresh_pool().await;
        let cipher = test_cipher();
        let connections: Arc<dyn EmailConnectionRepository> = Arc::new(
            PgEmailConnectionRepository::with_cipher(pool.clone(), Arc::clone(&cipher)),
        );
        let client = Arc::new(MockOAuthClient::new(true));
        let service = GmailOAuthService::new(
            client.clone(),
            Arc::new(PgGmailOAuthStateStore::new(pool.clone(), cipher)),
            Arc::clone(&connections),
            test_config(),
        );
        let user_id = Uuid::new_v4();
        let start = service.start(user_id).await.unwrap();
        assert!(!start.authorize_url.contains(&user_id.to_string()));

        let wrong_user = service
            .complete("valid-code", &start.state, Some(Uuid::new_v4()))
            .await
            .unwrap_err();
        assert!(matches!(
            wrong_user.downcast_ref::<OAuthFlowError>(),
            Some(OAuthFlowError::InvalidState)
        ));

        let connection = service
            .complete("valid-code", &start.state, Some(user_id))
            .await
            .unwrap();
        assert_eq!(connection.email_address, "alice@example.com");
        assert_eq!(connections.list_by_user(user_id).await.unwrap().len(), 1);
        assert!(
            client
                .last_verifier
                .lock()
                .unwrap()
                .as_deref()
                .is_some_and(|value| value.len() >= 43)
        );

        let replay = service
            .complete("valid-code", &start.state, Some(user_id))
            .await
            .unwrap_err();
        assert!(matches!(
            replay.downcast_ref::<OAuthFlowError>(),
            Some(OAuthFlowError::InvalidState)
        ));

        let reconnect = service.start(user_id).await.unwrap();
        let updated = service
            .complete("valid-code", &reconnect.state, Some(user_id))
            .await
            .unwrap();
        assert_eq!(updated.id, connection.id, "normalized address is upserted");

        service.disconnect(connection.id, user_id).await.unwrap();
        assert!(client.revoked.load(Ordering::SeqCst));
        assert!(connections.list_by_user(user_id).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn missing_refresh_token_is_rejected() {
        let pool = test_db::fresh_pool().await;
        let cipher = test_cipher();
        let connections: Arc<dyn EmailConnectionRepository> = Arc::new(
            PgEmailConnectionRepository::with_cipher(pool.clone(), Arc::clone(&cipher)),
        );
        let service = GmailOAuthService::new(
            Arc::new(MockOAuthClient::new(false)),
            Arc::new(PgGmailOAuthStateStore::new(pool, cipher)),
            connections,
            test_config(),
        );
        let user_id = Uuid::new_v4();
        let start = service.start(user_id).await.unwrap();
        let error = service
            .complete("valid-code", &start.state, Some(user_id))
            .await
            .unwrap_err();
        assert!(matches!(
            error.downcast_ref::<OAuthFlowError>(),
            Some(OAuthFlowError::MissingRefreshToken)
        ));
    }

    #[tokio::test]
    async fn expired_state_is_rejected_before_code_exchange() {
        let pool = test_db::fresh_pool().await;
        let cipher = test_cipher();
        let connections: Arc<dyn EmailConnectionRepository> = Arc::new(
            PgEmailConnectionRepository::with_cipher(pool.clone(), Arc::clone(&cipher)),
        );
        let client = Arc::new(MockOAuthClient::new(true));
        let service = GmailOAuthService::new(
            client.clone(),
            Arc::new(PgGmailOAuthStateStore::new(pool.clone(), cipher)),
            connections,
            test_config(),
        );
        let user_id = Uuid::new_v4();
        let start = service.start(user_id).await.unwrap();
        sqlx::query("UPDATE gmail_oauth_states SET expires_at = now() - interval '1 second'")
            .execute(&pool)
            .await
            .unwrap();

        let error = service
            .complete("valid-code", &start.state, Some(user_id))
            .await
            .unwrap_err();
        assert!(matches!(
            error.downcast_ref::<OAuthFlowError>(),
            Some(OAuthFlowError::InvalidState)
        ));
        assert!(client.last_verifier.lock().unwrap().is_none());
    }

    #[tokio::test]
    async fn reconnect_without_new_refresh_token_retains_existing_secret() {
        let pool = test_db::fresh_pool().await;
        let cipher = test_cipher();
        let connections: Arc<dyn EmailConnectionRepository> = Arc::new(
            PgEmailConnectionRepository::with_cipher(pool.clone(), Arc::clone(&cipher)),
        );
        let first_service = GmailOAuthService::new(
            Arc::new(MockOAuthClient::new(true)),
            Arc::new(PgGmailOAuthStateStore::new(
                pool.clone(),
                Arc::clone(&cipher),
            )),
            Arc::clone(&connections),
            test_config(),
        );
        let user_id = Uuid::new_v4();
        let first_start = first_service.start(user_id).await.unwrap();
        let first = first_service
            .complete("valid-code", &first_start.state, Some(user_id))
            .await
            .unwrap();

        let reconnect_service = GmailOAuthService::new(
            Arc::new(MockOAuthClient::new(false)),
            Arc::new(PgGmailOAuthStateStore::new(pool, cipher)),
            Arc::clone(&connections),
            test_config(),
        );
        let reconnect_start = reconnect_service.start(user_id).await.unwrap();
        let reconnected = reconnect_service
            .complete("valid-code", &reconnect_start.state, Some(user_id))
            .await
            .unwrap();

        assert_eq!(reconnected.id, first.id);
        assert_eq!(reconnected.oauth_refresh_token.expose(), "refresh-token");
        assert_eq!(connections.list_by_user(user_id).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn unverified_profile_is_rejected() {
        let pool = test_db::fresh_pool().await;
        let cipher = test_cipher();
        let connections: Arc<dyn EmailConnectionRepository> = Arc::new(
            PgEmailConnectionRepository::with_cipher(pool.clone(), Arc::clone(&cipher)),
        );
        let service = GmailOAuthService::new(
            Arc::new(MockOAuthClient::new(true).with_unverified_email()),
            Arc::new(PgGmailOAuthStateStore::new(pool, cipher)),
            connections,
            test_config(),
        );
        let user_id = Uuid::new_v4();
        let start = service.start(user_id).await.unwrap();
        let error = service
            .complete("valid-code", &start.state, Some(user_id))
            .await
            .unwrap_err();
        assert!(matches!(
            error.downcast_ref::<OAuthFlowError>(),
            Some(OAuthFlowError::InvalidProviderCredentials)
        ));
    }
}
