use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::Engine;
use rand::RngCore;
use rand::rngs::OsRng;
use sqlx::PgPool;
use uuid::Uuid;
use zeroize::Zeroize;

pub use crate::domain::secret::SecretString as SecretValue;

const ENVELOPE_PREFIX: &str = "enc:v1";
pub const LEGACY_CREDENTIAL_SENTINEL: &str = "[encrypted]";

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum CredentialWriteMode {
    /// Transitional rollout mode: retain the legacy plaintext value while also
    /// writing the encrypted envelope.
    #[default]
    DualWrite,
    /// Post-backfill mode: only the encrypted column contains the credential.
    EncryptedOnly,
}

impl CredentialWriteMode {
    fn from_env() -> anyhow::Result<Self> {
        match std::env::var("TOKEN_ENCRYPTION_WRITE_MODE")
            .or_else(|_| std::env::var("CREDENTIAL_ENCRYPTION_WRITE_MODE"))
            .unwrap_or_else(|_| "dual_write".to_string())
            .as_str()
        {
            "dual_write" => Ok(Self::DualWrite),
            "encrypted_only" => Ok(Self::EncryptedOnly),
            value => anyhow::bail!(
                "unsupported TOKEN_ENCRYPTION_WRITE_MODE {value:?}; expected dual_write or encrypted_only"
            ),
        }
    }
}

#[derive(Clone)]
pub struct TokenCipherConfig {
    pub active_key_id: String,
    pub keys: HashMap<String, [u8; 32]>,
}

impl fmt::Debug for TokenCipherConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TokenCipherConfig")
            .field("active_key_id", &self.active_key_id)
            .field("key_ids", &self.keys.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl Drop for TokenCipherConfig {
    fn drop(&mut self) {
        for key in self.keys.values_mut() {
            key.zeroize();
        }
    }
}

impl TokenCipherConfig {
    /// Parses `TOKEN_ENCRYPTION_KEYS` as comma-separated
    /// `key-id:base64-encoded-32-byte-key` entries. The active key is selected
    /// by `TOKEN_ENCRYPTION_PRIMARY_KEY_ID`. The earlier
    /// `CREDENTIAL_ENCRYPTION_*` names remain accepted for rollout
    /// compatibility.
    pub fn from_env() -> anyhow::Result<Self> {
        let active_key_id = std::env::var("TOKEN_ENCRYPTION_PRIMARY_KEY_ID")
            .or_else(|_| std::env::var("CREDENTIAL_ENCRYPTION_ACTIVE_KEY_ID"))
            .map_err(|_| anyhow::anyhow!("TOKEN_ENCRYPTION_PRIMARY_KEY_ID must be set"))?;
        let encoded = std::env::var("TOKEN_ENCRYPTION_KEYS")
            .or_else(|_| std::env::var("CREDENTIAL_ENCRYPTION_KEYS"))
            .map_err(|_| anyhow::anyhow!("TOKEN_ENCRYPTION_KEYS must be set"))?;
        Self::parse(active_key_id, &encoded)
    }

    pub fn parse(active_key_id: String, encoded: &str) -> anyhow::Result<Self> {
        let mut keys = HashMap::new();
        for entry in encoded.split(',').filter(|value| !value.trim().is_empty()) {
            let (key_id, key) = entry
                .split_once(':')
                .or_else(|| entry.split_once('='))
                .ok_or_else(|| anyhow::anyhow!("invalid credential key entry"))?;
            let key_id = key_id.trim();
            if key_id.is_empty() || key_id.contains(':') {
                anyhow::bail!("credential key id must be non-empty and cannot contain ':'");
            }
            let decoded = base64::engine::general_purpose::STANDARD
                .decode(key.trim())
                .map_err(|_| anyhow::anyhow!("credential key {key_id} is not valid base64"))?;
            let key: [u8; 32] = decoded.try_into().map_err(|_| {
                anyhow::anyhow!("credential key {key_id} must decode to exactly 32 bytes")
            })?;
            if keys.insert(key_id.to_string(), key).is_some() {
                anyhow::bail!("duplicate credential key id: {key_id}");
            }
        }
        if !keys.contains_key(&active_key_id) {
            anyhow::bail!("active credential key id is not present in keyring");
        }
        Ok(Self {
            active_key_id,
            keys,
        })
    }
}

pub struct TokenCipher {
    config: TokenCipherConfig,
    write_mode: CredentialWriteMode,
}

impl fmt::Debug for TokenCipher {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TokenCipher")
            .field("active_key_id", &self.config.active_key_id)
            .field("write_mode", &self.write_mode)
            .finish_non_exhaustive()
    }
}

impl TokenCipher {
    pub fn new(config: TokenCipherConfig) -> Self {
        Self::with_write_mode(config, CredentialWriteMode::DualWrite)
    }

    pub fn with_write_mode(config: TokenCipherConfig, write_mode: CredentialWriteMode) -> Self {
        Self { config, write_mode }
    }

    pub fn from_env() -> anyhow::Result<Self> {
        Ok(Self::with_write_mode(
            TokenCipherConfig::from_env()?,
            CredentialWriteMode::from_env()?,
        ))
    }

    pub fn active_key_id(&self) -> &str {
        &self.config.active_key_id
    }

    pub fn write_mode(&self) -> CredentialWriteMode {
        self.write_mode
    }

    pub fn legacy_write_value(&self, plaintext: &str) -> SecretValue {
        SecretValue::new(match self.write_mode {
            CredentialWriteMode::DualWrite => plaintext.to_string(),
            CredentialWriteMode::EncryptedOnly => LEGACY_CREDENTIAL_SENTINEL.to_string(),
        })
    }

    pub fn encrypt(&self, plaintext: &str, associated_data: &[u8]) -> anyhow::Result<String> {
        let key = self
            .config
            .keys
            .get(&self.config.active_key_id)
            .expect("active key validated by TokenCipherConfig");
        let cipher = Aes256Gcm::new_from_slice(key)
            .map_err(|_| anyhow::anyhow!("invalid AES-256-GCM key"))?;
        let mut nonce_bytes = [0_u8; 12];
        OsRng.fill_bytes(&mut nonce_bytes);
        let ciphertext = cipher
            .encrypt(
                Nonce::from_slice(&nonce_bytes),
                aes_gcm::aead::Payload {
                    msg: plaintext.as_bytes(),
                    aad: associated_data,
                },
            )
            .map_err(|_| anyhow::anyhow!("credential encryption failed"))?;
        let mut payload = Vec::with_capacity(nonce_bytes.len() + ciphertext.len());
        payload.extend_from_slice(&nonce_bytes);
        payload.extend_from_slice(&ciphertext);
        Ok(format!(
            "{ENVELOPE_PREFIX}:{}:{}",
            self.config.active_key_id,
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(payload)
        ))
    }

    pub fn decrypt(&self, envelope: &str, associated_data: &[u8]) -> anyhow::Result<SecretValue> {
        let mut pieces = envelope.splitn(4, ':');
        let marker = pieces.next();
        let version = pieces.next();
        let key_id = pieces.next();
        let payload = pieces.next();
        if marker != Some("enc") || version != Some("v1") {
            anyhow::bail!("unsupported credential envelope version");
        }
        let key_id = key_id.ok_or_else(|| anyhow::anyhow!("invalid credential envelope"))?;
        let payload = payload.ok_or_else(|| anyhow::anyhow!("invalid credential envelope"))?;
        let key = self
            .config
            .keys
            .get(key_id)
            .ok_or_else(|| anyhow::anyhow!("credential key id is unavailable: {key_id}"))?;
        let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(payload)
            .map_err(|_| anyhow::anyhow!("invalid credential envelope encoding"))?;
        if decoded.len() < 12 + 16 {
            anyhow::bail!("invalid credential envelope length");
        }
        let (nonce, ciphertext) = decoded.split_at(12);
        let cipher = Aes256Gcm::new_from_slice(key)
            .map_err(|_| anyhow::anyhow!("invalid AES-256-GCM key"))?;
        let plaintext = cipher
            .decrypt(
                Nonce::from_slice(nonce),
                aes_gcm::aead::Payload {
                    msg: ciphertext,
                    aad: associated_data,
                },
            )
            .map_err(|_| anyhow::anyhow!("credential decryption failed"))?;
        let plaintext = String::from_utf8(plaintext)
            .map_err(|_| anyhow::anyhow!("decrypted credential is not UTF-8"))?;
        Ok(SecretValue::new(plaintext))
    }

    pub fn needs_rotation(&self, envelope: &str) -> bool {
        envelope
            .split(':')
            .nth(2)
            .is_none_or(|key_id| key_id != self.active_key_id())
    }
}

pub fn bank_token_aad(id: Uuid) -> String {
    format!("bank_connections:{id}:token")
}

pub fn email_access_token_aad(id: Uuid) -> String {
    format!("email_connections:{id}:oauth_access_token")
}

pub fn email_refresh_token_aad(id: Uuid) -> String {
    format!("email_connections:{id}:oauth_refresh_token")
}

pub fn oauth_pkce_aad(state_hash: &[u8]) -> Vec<u8> {
    let mut aad = b"gmail_oauth_states:".to_vec();
    aad.extend_from_slice(state_hash);
    aad.extend_from_slice(b":pkce_verifier");
    aad
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct CredentialRotationReport {
    pub bank_connections: u64,
    pub email_connections: u64,
}

/// Backfills plaintext credentials and rewraps envelopes encrypted with older
/// keys. Plaintext remains dual-written until `sanitize_plaintext` is invoked
/// as a separate, explicit post-verification step. Running it repeatedly is safe.
pub struct CredentialRotationService {
    pool: PgPool,
    cipher: Arc<TokenCipher>,
}

impl CredentialRotationService {
    pub fn new(pool: PgPool, cipher: Arc<TokenCipher>) -> Self {
        Self { pool, cipher }
    }

    pub async fn run(&self) -> anyhow::Result<CredentialRotationReport> {
        #[derive(sqlx::FromRow)]
        struct BankRow {
            id: Uuid,
            token: String,
            token_encrypted: Option<String>,
        }
        #[derive(sqlx::FromRow)]
        struct EmailRow {
            id: Uuid,
            oauth_access_token: String,
            oauth_refresh_token: String,
            oauth_access_token_encrypted: Option<String>,
            oauth_refresh_token_encrypted: Option<String>,
        }

        let mut tx = self.pool.begin().await?;
        let banks = sqlx::query_as::<_, BankRow>(
            "SELECT id, token, token_encrypted FROM bank_connections FOR UPDATE",
        )
        .fetch_all(&mut *tx)
        .await?;
        let mut report = CredentialRotationReport::default();
        for row in banks {
            let should_write = row
                .token_encrypted
                .as_deref()
                .is_none_or(|value| self.cipher.needs_rotation(value));
            if !should_write {
                continue;
            }
            let aad = bank_token_aad(row.id);
            let plaintext = match row.token_encrypted {
                Some(value) => self.cipher.decrypt(&value, aad.as_bytes())?,
                None => SecretValue::new(row.token),
            };
            let encrypted = self.cipher.encrypt(plaintext.expose(), aad.as_bytes())?;
            sqlx::query("UPDATE bank_connections SET token_encrypted=$1 WHERE id=$2")
                .bind(encrypted)
                .bind(row.id)
                .execute(&mut *tx)
                .await?;
            report.bank_connections += 1;
        }

        let emails = sqlx::query_as::<_, EmailRow>(
            "SELECT id, oauth_access_token, oauth_refresh_token, \
                    oauth_access_token_encrypted, oauth_refresh_token_encrypted \
             FROM email_connections FOR UPDATE",
        )
        .fetch_all(&mut *tx)
        .await?;
        for row in emails {
            let access_aad = email_access_token_aad(row.id);
            let refresh_aad = email_refresh_token_aad(row.id);
            let should_write = row
                .oauth_access_token_encrypted
                .as_deref()
                .is_none_or(|value| self.cipher.needs_rotation(value))
                || row
                    .oauth_refresh_token_encrypted
                    .as_deref()
                    .is_none_or(|value| self.cipher.needs_rotation(value));
            if !should_write {
                continue;
            }
            let access = match row.oauth_access_token_encrypted {
                Some(value) => self.cipher.decrypt(&value, access_aad.as_bytes())?,
                None => SecretValue::new(row.oauth_access_token),
            };
            let refresh = match row.oauth_refresh_token_encrypted {
                Some(value) => self.cipher.decrypt(&value, refresh_aad.as_bytes())?,
                None => SecretValue::new(row.oauth_refresh_token),
            };
            let access_encrypted = self
                .cipher
                .encrypt(access.expose(), access_aad.as_bytes())?;
            let refresh_encrypted = self
                .cipher
                .encrypt(refresh.expose(), refresh_aad.as_bytes())?;
            sqlx::query(
                "UPDATE email_connections SET oauth_access_token_encrypted=$1, \
                    oauth_refresh_token_encrypted=$2 WHERE id=$3",
            )
            .bind(access_encrypted)
            .bind(refresh_encrypted)
            .bind(row.id)
            .execute(&mut *tx)
            .await?;
            report.email_connections += 1;
        }
        tx.commit().await?;
        Ok(report)
    }

    /// Replaces legacy plaintext columns only after verifying every ciphertext
    /// can be decrypted with the configured keyring. This is intentionally not
    /// part of `run`, so rollout can verify dual-read behavior first.
    pub async fn sanitize_plaintext(&self) -> anyhow::Result<CredentialRotationReport> {
        if self.cipher.write_mode() != CredentialWriteMode::EncryptedOnly {
            anyhow::bail!(
                "plaintext sanitization requires TOKEN_ENCRYPTION_WRITE_MODE=encrypted_only"
            );
        }
        #[derive(sqlx::FromRow)]
        struct BankRow {
            id: Uuid,
            token_encrypted: Option<String>,
        }
        #[derive(sqlx::FromRow)]
        struct EmailRow {
            id: Uuid,
            oauth_access_token_encrypted: Option<String>,
            oauth_refresh_token_encrypted: Option<String>,
        }

        let mut tx = self.pool.begin().await?;
        let banks = sqlx::query_as::<_, BankRow>(
            "SELECT id, token_encrypted FROM bank_connections FOR UPDATE",
        )
        .fetch_all(&mut *tx)
        .await?;
        for row in &banks {
            let encrypted = row.token_encrypted.as_deref().ok_or_else(|| {
                anyhow::anyhow!("bank connection {} has not been backfilled", row.id)
            })?;
            let aad = bank_token_aad(row.id);
            self.cipher.decrypt(encrypted, aad.as_bytes())?;
        }

        let emails = sqlx::query_as::<_, EmailRow>(
            "SELECT id, oauth_access_token_encrypted, oauth_refresh_token_encrypted \
             FROM email_connections FOR UPDATE",
        )
        .fetch_all(&mut *tx)
        .await?;
        for row in &emails {
            let access = row.oauth_access_token_encrypted.as_deref().ok_or_else(|| {
                anyhow::anyhow!(
                    "email connection {} access token has not been backfilled",
                    row.id
                )
            })?;
            let refresh = row
                .oauth_refresh_token_encrypted
                .as_deref()
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "email connection {} refresh token has not been backfilled",
                        row.id
                    )
                })?;
            self.cipher
                .decrypt(access, email_access_token_aad(row.id).as_bytes())?;
            self.cipher
                .decrypt(refresh, email_refresh_token_aad(row.id).as_bytes())?;
        }

        let bank_count = sqlx::query("UPDATE bank_connections SET token=$1 WHERE token<>$1")
            .bind(LEGACY_CREDENTIAL_SENTINEL)
            .execute(&mut *tx)
            .await?
            .rows_affected();
        let email_count = sqlx::query(
            "UPDATE email_connections SET oauth_access_token=$1, oauth_refresh_token=$1 \
             WHERE oauth_access_token<>$1 OR oauth_refresh_token<>$1",
        )
        .bind(LEGACY_CREDENTIAL_SENTINEL)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        tx.commit().await?;
        Ok(CredentialRotationReport {
            bank_connections: bank_count,
            email_connections: email_count,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::email_connection::{
        EmailConnection, EmailConnectionRepository, EmailConnectionStatus, EmailProvider,
    };
    use crate::infrastructure::email_connection_repository::PgEmailConnectionRepository;
    use crate::infrastructure::test_db;
    use chrono::Utc;

    fn cipher(active: &str, keys: &[(&str, u8)]) -> TokenCipher {
        TokenCipher::new(TokenCipherConfig {
            active_key_id: active.to_string(),
            keys: keys
                .iter()
                .map(|(id, byte)| ((*id).to_string(), [*byte; 32]))
                .collect(),
        })
    }

    #[test]
    fn encrypt_roundtrip_is_versioned_and_randomized() {
        let cipher = cipher("k1", &[("k1", 7)]);
        let first = cipher.encrypt("secret", b"row:1").unwrap();
        let second = cipher.encrypt("secret", b"row:1").unwrap();
        assert!(first.starts_with("enc:v1:k1:"));
        assert_ne!(first, second);
        assert_eq!(cipher.decrypt(&first, b"row:1").unwrap().expose(), "secret");
        assert!(cipher.decrypt(&first, b"row:2").is_err());
        assert!(!format!("{:?}", cipher.decrypt(&first, b"row:1").unwrap()).contains("secret"));
    }

    #[test]
    fn old_key_decrypts_and_requests_rotation() {
        let old = cipher("old", &[("old", 1)]);
        let envelope = old.encrypt("secret", b"row").unwrap();
        let rotated = cipher("new", &[("old", 1), ("new", 2)]);
        assert_eq!(
            rotated.decrypt(&envelope, b"row").unwrap().expose(),
            "secret"
        );
        assert!(rotated.needs_rotation(&envelope));
    }

    #[test]
    fn config_debug_redacts_key_bytes() {
        let config = TokenCipherConfig {
            active_key_id: "k1".to_string(),
            keys: [("k1".to_string(), [99; 32])].into_iter().collect(),
        };
        let output = format!("{config:?}");
        assert!(output.contains("k1"));
        assert!(!output.contains("99"));
    }

    #[test]
    fn encrypted_only_mode_never_returns_plaintext_for_legacy_columns() {
        let cipher = TokenCipher::with_write_mode(
            TokenCipherConfig {
                active_key_id: "k1".to_string(),
                keys: [("k1".to_string(), [9; 32])].into_iter().collect(),
            },
            CredentialWriteMode::EncryptedOnly,
        );
        assert_eq!(cipher.write_mode(), CredentialWriteMode::EncryptedOnly);
        assert_eq!(
            cipher.legacy_write_value("must-not-be-stored"),
            LEGACY_CREDENTIAL_SENTINEL
        );
    }

    #[tokio::test]
    async fn backfill_retains_plaintext_then_explicit_sanitize_removes_it() {
        let pool = test_db::fresh_pool().await;
        let user_id = Uuid::new_v4();
        let connection = EmailConnection {
            id: Uuid::new_v4(),
            user_id,
            provider: EmailProvider::Gmail,
            email_address: "backfill@example.com".to_string(),
            oauth_access_token: "legacy-access".into(),
            oauth_refresh_token: "legacy-refresh".into(),
            credential_version: 0,
            access_token_expires_at: Utc::now(),
            status: EmailConnectionStatus::Connected,
            last_synced_at: None,
            last_history_id: None,
            created_at: Utc::now(),
        };
        PgEmailConnectionRepository::new(pool.clone())
            .create(&connection)
            .await
            .unwrap();
        let cipher = Arc::new(cipher("active", &[("active", 4)]));
        let maintenance = CredentialRotationService::new(pool.clone(), Arc::clone(&cipher));
        assert_eq!(maintenance.run().await.unwrap().email_connections, 1);

        let dual: (String, Option<String>) = sqlx::query_as(
            "SELECT oauth_access_token, oauth_access_token_encrypted \
             FROM email_connections WHERE id=$1",
        )
        .bind(connection.id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(dual.0, "legacy-access");
        assert!(dual.1.is_some());

        assert!(maintenance.sanitize_plaintext().await.is_err());
        let encrypted_only = Arc::new(TokenCipher::with_write_mode(
            TokenCipherConfig {
                active_key_id: "active".to_string(),
                keys: [("active".to_string(), [4; 32])].into_iter().collect(),
            },
            CredentialWriteMode::EncryptedOnly,
        ));
        let sanitization =
            CredentialRotationService::new(pool.clone(), Arc::clone(&encrypted_only));
        assert_eq!(
            sanitization
                .sanitize_plaintext()
                .await
                .unwrap()
                .email_connections,
            1
        );
        let sanitized: (String, String) = sqlx::query_as(
            "SELECT oauth_access_token, oauth_refresh_token \
             FROM email_connections WHERE id=$1",
        )
        .bind(connection.id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(sanitized.0, LEGACY_CREDENTIAL_SENTINEL);
        assert_eq!(sanitized.1, LEGACY_CREDENTIAL_SENTINEL);
        let decrypted = PgEmailConnectionRepository::with_cipher(pool, encrypted_only)
            .find_by_id(connection.id, user_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(decrypted.oauth_access_token, "legacy-access");
        assert_eq!(decrypted.oauth_refresh_token, "legacy-refresh");
    }
}
