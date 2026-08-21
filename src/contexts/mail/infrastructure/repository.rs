use super::super::{
    application::ports::{GmailOAuth, OAuthTokens},
    domain::{ConnectionState, ConnectionVersion, GmailConnectionId},
    public::ConnectionView,
};
use crate::shared_kernel::UserId;
use aes_gcm::{
    Aes256Gcm, KeyInit, Nonce,
    aead::{Aead, Payload},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Utc};
use rand::{RngCore, rngs::OsRng};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row};
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub(crate) enum MailStoreError {
    #[error("mail item was not found")]
    NotFound,
    #[error("mail version conflict")]
    VersionConflict,
    #[error("mail idempotency conflict")]
    IdempotencyConflict,
    #[error("OAuth state is invalid or expired")]
    InvalidOauthState,
    #[error("OAuth provider request failed")]
    OAuthProvider,
    #[error(transparent)]
    Database(#[from] sqlx::Error),
}
#[derive(Clone, Debug)]
pub(crate) struct OauthStartResult {
    pub response: Value,
    pub replayed: bool,
}
#[derive(Clone, Debug)]
pub(crate) struct CallbackResult {
    pub response: Value,
    pub replayed: bool,
}
#[derive(Clone, Debug)]
pub(crate) enum OauthCallbackPreparation {
    Replay(CallbackResult),
    Exchange { verifier: String },
}

#[derive(Serialize, Deserialize)]
pub(crate) struct EncryptedOAuthCredential {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at: DateTime<Utc>,
}
#[derive(Clone)]
pub(crate) struct PgMailStore {
    pool: PgPool,
}
impl PgMailStore {
    pub(crate) fn new(pool: PgPool) -> Self {
        Self { pool }
    }
    pub(crate) async fn list_connections(
        &self,
        user: UserId,
    ) -> Result<Vec<ConnectionView>, sqlx::Error> {
        let rows=sqlx::query("SELECT id,state,version,credential_generation,sync_generation FROM mail.connections WHERE user_id=$1 ORDER BY created_at,id").bind(user.into_uuid()).fetch_all(&self.pool).await?;
        rows.into_iter().map(row_view).collect()
    }
    pub(crate) async fn get_connection(
        &self,
        user: UserId,
        id: GmailConnectionId,
    ) -> Result<Option<ConnectionView>, sqlx::Error> {
        sqlx::query("SELECT id,state,version,credential_generation,sync_generation FROM mail.connections WHERE user_id=$1 AND id=$2").bind(user.into_uuid()).bind(id.into_uuid()).fetch_optional(&self.pool).await?.map(row_view).transpose()
    }
    pub(crate) async fn disconnect(
        &self,
        user: UserId,
        id: GmailConnectionId,
        expected: ConnectionVersion,
        now: DateTime<Utc>,
    ) -> Result<Option<ConnectionVersion>, sqlx::Error> {
        let mut tx = self.pool.begin().await?;
        let row=sqlx::query("UPDATE mail.connections SET state='disconnected',credential_ciphertext=NULL,credential_nonce=NULL,credential_key_id=NULL,credential_generation=credential_generation+1,sync_generation=sync_generation+1,version=version+1,updated_at=$4 WHERE user_id=$1 AND id=$2 AND version=$3 RETURNING version").bind(user.into_uuid()).bind(id.into_uuid()).bind(i64::try_from(expected.get()).unwrap_or(i64::MAX)).bind(now).fetch_optional(&mut *tx).await?;
        if row.is_some() {
            cancel_connection_jobs(&mut tx, user.into_uuid(), id.into_uuid(), now).await?;
        }
        tx.commit().await?;
        row.map(|r| {
            ConnectionVersion::new(u64::try_from(r.get::<i64, _>("version")).unwrap_or_default())
                .map_err(|_| sqlx::Error::Protocol("invalid mail version".into()))
        })
        .transpose()
    }
    pub(crate) async fn connection_status(
        &self,
        user: UserId,
        id: GmailConnectionId,
    ) -> Result<Option<Value>, sqlx::Error> {
        let connection = self.get_connection(user, id).await?;
        let Some(connection) = connection else {
            return Ok(None);
        };
        let job = sqlx::query("SELECT id,state,cursor,attempts,next_retry_at,last_error,created_at,updated_at FROM mail.sync_jobs WHERE user_id=$1 AND connection_id=$2 ORDER BY created_at DESC,id DESC LIMIT 1")
            .bind(user.into_uuid()).bind(id.into_uuid()).fetch_optional(&self.pool).await?;
        Ok(Some(json!({
            "connection": connection,
            "sync": job.map(|row| json!({
                "job_id":row.get::<Uuid,_>("id"),"state":row.get::<String,_>("state"),
                "cursor":row.get::<Option<String>,_>("cursor"),"attempts":row.get::<i32,_>("attempts"),
                "next_retry_at":row.get::<Option<DateTime<Utc>>,_>("next_retry_at"),
                "last_error":row.get::<Option<String>,_>("last_error"),
                "created_at":row.get::<DateTime<Utc>,_>("created_at"),"updated_at":row.get::<DateTime<Utc>,_>("updated_at")
            }))
        })))
    }
    pub(crate) async fn request_resync(
        &self,
        user: UserId,
        id: GmailConnectionId,
        expected: ConnectionVersion,
        now: DateTime<Utc>,
    ) -> Result<Option<(Uuid, ConnectionVersion)>, sqlx::Error> {
        let mut tx = self.pool.begin().await?;
        let row=sqlx::query("UPDATE mail.connections SET sync_generation=sync_generation+1,version=version+1,updated_at=$4 WHERE user_id=$1 AND id=$2 AND version=$3 AND state='active' RETURNING version,credential_generation,sync_generation").bind(user.into_uuid()).bind(id.into_uuid()).bind(i64::try_from(expected.get()).unwrap_or(i64::MAX)).bind(now).fetch_optional(&mut *tx).await?;
        let Some(row) = row else {
            tx.rollback().await?;
            return Ok(None);
        };
        cancel_connection_jobs(&mut tx, user.into_uuid(), id.into_uuid(), now).await?;
        let job = Uuid::new_v4();
        sqlx::query("INSERT INTO mail.sync_jobs(id,user_id,connection_id,state,connection_version,credential_generation,sync_generation,created_at,updated_at) VALUES($1,$2,$3,'requested',$4,$5,$6,$7,$7)").bind(job).bind(user.into_uuid()).bind(id.into_uuid()).bind(row.get::<i64,_>("version")).bind(row.get::<i64,_>("credential_generation")).bind(row.get::<i64,_>("sync_generation")).bind(now).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(Some((
            job,
            ConnectionVersion::new(u64::try_from(row.get::<i64, _>("version")).unwrap_or_default())
                .map_err(|_| sqlx::Error::Protocol("invalid mail version".into()))?,
        )))
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn disconnect_command(
        &self,
        user: UserId,
        id: Uuid,
        expected: u64,
        key: &str,
        hash: [u8; 32],
        now: DateTime<Utc>,
    ) -> Result<Value, MailStoreError> {
        let mut tx = self.pool.begin().await?;
        if let Some(response) = claim_mail_receipt(
            &mut tx,
            user,
            "disconnect_email_connection",
            key,
            "disconnect_email_connection",
            Some(id),
            hash,
            now,
        )
        .await?
        {
            tx.commit().await?;
            return Ok(response);
        }
        let row=sqlx::query("UPDATE mail.connections SET state='disconnected',credential_ciphertext=NULL,credential_nonce=NULL,credential_key_id=NULL,credential_generation=credential_generation+1,sync_generation=sync_generation+1,version=version+1,updated_at=$4 WHERE user_id=$1 AND id=$2 AND version=$3 AND state<>'disconnected' RETURNING version").bind(user.into_uuid()).bind(id).bind(i64::try_from(expected).unwrap_or(i64::MAX)).bind(now).fetch_optional(&mut *tx).await?;
        let Some(row) = row else {
            reject_mail_receipt(
                &mut tx,
                user,
                "disconnect_email_connection",
                key,
                "version_conflict",
                409,
                now,
            )
            .await?;
            tx.commit().await?;
            return Err(MailStoreError::VersionConflict);
        };
        cancel_connection_jobs(&mut tx, user.into_uuid(), id, now).await?;
        let version: i64 = row.get("version");
        let response = json!({"connection_id":id,"status":"disconnected","version":version});
        finish_mail_receipt(
            &mut tx,
            user,
            "disconnect_email_connection",
            key,
            200,
            &response,
            id,
            version,
            now,
        )
        .await?;
        tx.commit().await?;
        Ok(response)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn resync_command(
        &self,
        user: UserId,
        id: Uuid,
        expected: u64,
        key: &str,
        hash: [u8; 32],
        now: DateTime<Utc>,
    ) -> Result<Value, MailStoreError> {
        let mut tx = self.pool.begin().await?;
        if let Some(response) = claim_mail_receipt(
            &mut tx,
            user,
            "resync_email_connection",
            key,
            "resync_email_connection",
            Some(id),
            hash,
            now,
        )
        .await?
        {
            tx.commit().await?;
            return Ok(response);
        }
        let row=sqlx::query("UPDATE mail.connections SET sync_generation=sync_generation+1,version=version+1,updated_at=$4 WHERE user_id=$1 AND id=$2 AND version=$3 AND state='active' RETURNING version,credential_generation,sync_generation").bind(user.into_uuid()).bind(id).bind(i64::try_from(expected).unwrap_or(i64::MAX)).bind(now).fetch_optional(&mut *tx).await?;
        let Some(row) = row else {
            reject_mail_receipt(
                &mut tx,
                user,
                "resync_email_connection",
                key,
                "version_conflict",
                409,
                now,
            )
            .await?;
            tx.commit().await?;
            return Err(MailStoreError::VersionConflict);
        };
        cancel_connection_jobs(&mut tx, user.into_uuid(), id, now).await?;
        let job = Uuid::new_v4();
        let version: i64 = row.get("version");
        sqlx::query("INSERT INTO mail.sync_jobs(id,user_id,connection_id,state,connection_version,credential_generation,sync_generation,created_at,updated_at) VALUES($1,$2,$3,'requested',$4,$5,$6,$7,$7)").bind(job).bind(user.into_uuid()).bind(id).bind(version).bind(row.get::<i64,_>("credential_generation")).bind(row.get::<i64,_>("sync_generation")).bind(now).execute(&mut *tx).await?;
        let response = json!({"job_id":job,"status":"requested","connection_version":version});
        finish_mail_receipt(
            &mut tx,
            user,
            "resync_email_connection",
            key,
            202,
            &response,
            id,
            version,
            now,
        )
        .await?;
        tx.commit().await?;
        Ok(response)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn start_oauth(
        &self,
        user: UserId,
        replacement: Option<Uuid>,
        expected: Option<u64>,
        key: &str,
        hash: [u8; 32],
        now: DateTime<Utc>,
        oauth_provider: &dyn GmailOAuth,
    ) -> Result<OauthStartResult, MailStoreError> {
        let mut tx = self.pool.begin().await?;
        let inserted=sqlx::query("INSERT INTO mail.command_receipts(user_id,command_scope,idempotency_key,command_name,target_id,request_hash,status,created_at) VALUES($1,'gmail_oauth_start',$2,'gmail_oauth_start',$3,$4,'processing',$5) ON CONFLICT DO NOTHING").bind(user.into_uuid()).bind(key).bind(replacement).bind(hash.as_slice()).bind(now).execute(&mut *tx).await?;
        if inserted.rows_affected() == 0 {
            let row=sqlx::query("SELECT request_hash,status,response_body FROM mail.command_receipts WHERE user_id=$1 AND command_scope='gmail_oauth_start' AND idempotency_key=$2").bind(user.into_uuid()).bind(key).fetch_one(&mut *tx).await?;
            if row.get::<Vec<u8>, _>("request_hash") != hash {
                return Err(MailStoreError::IdempotencyConflict);
            }
            let status: String = row.get("status");
            if status == "rejected" {
                let response: Value = row.try_get("response_body")?;
                return Err(
                    if response.get("error").and_then(Value::as_str) == Some("not_found") {
                        MailStoreError::NotFound
                    } else {
                        MailStoreError::VersionConflict
                    },
                );
            }
            if status != "succeeded" {
                return Err(MailStoreError::VersionConflict);
            }
            let response = row.get("response_body");
            tx.commit().await?;
            return Ok(OauthStartResult {
                response,
                replayed: true,
            });
        }
        if let Some(connection_id) = replacement {
            let current:Option<i64>=sqlx::query_scalar("SELECT version FROM mail.connections WHERE user_id=$1 AND id=$2 AND state<>'disconnected' FOR UPDATE").bind(user.into_uuid()).bind(connection_id).fetch_optional(&mut *tx).await?;
            let Some(current) = current else {
                sqlx::query("UPDATE mail.command_receipts SET status='rejected',http_status=404,response_body=$3,completed_at=$4 WHERE user_id=$1 AND command_scope='gmail_oauth_start' AND idempotency_key=$2")
                    .bind(user.into_uuid()).bind(key).bind(json!({"error":"not_found"})).bind(now)
                    .execute(&mut *tx).await?;
                tx.commit().await?;
                return Err(MailStoreError::NotFound);
            };
            if Some(u64::try_from(current).unwrap_or(u64::MAX)) != expected {
                sqlx::query("UPDATE mail.command_receipts SET status='rejected',http_status=409,response_body=$3,completed_at=$4 WHERE user_id=$1 AND command_scope='gmail_oauth_start' AND idempotency_key=$2")
                    .bind(user.into_uuid()).bind(key).bind(json!({"error":"version_conflict"})).bind(now)
                    .execute(&mut *tx).await?;
                tx.commit().await?;
                return Err(MailStoreError::VersionConflict);
            }
        }
        let state = Uuid::new_v4().to_string();
        let state_digest: [u8; 32] = Sha256::digest(state.as_bytes()).into();
        let mut verifier_bytes = [0_u8; 32];
        OsRng.fill_bytes(&mut verifier_bytes);
        let verifier = URL_SAFE_NO_PAD.encode(verifier_bytes);
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        let (ciphertext, nonce) = encrypt(verifier.as_bytes(), &state_digest)?;
        sqlx::query("INSERT INTO mail.oauth_states(state_digest,user_id,verifier_ciphertext,verifier_nonce,key_id,replacement_connection_id,expected_version,expires_at,created_at) VALUES($1,$2,$3,$4,'parallel-v2-mail',$5,$6,$7,$8)").bind(state_digest.as_slice()).bind(user.into_uuid()).bind(ciphertext).bind(nonce).bind(replacement).bind(expected.map(|v|i64::try_from(v).unwrap_or(i64::MAX))).bind(now+chrono::Duration::minutes(10)).bind(now).execute(&mut *tx).await?;
        let authorization_url = oauth_provider
            .authorization_url(&state, &challenge)
            .map_err(|_| MailStoreError::OAuthProvider)?;
        let response = json!({"authorization_url":authorization_url,"expires_in":600});
        sqlx::query("UPDATE mail.command_receipts SET status='succeeded',http_status=200,response_body=$3,completed_at=$4 WHERE user_id=$1 AND command_scope='gmail_oauth_start' AND idempotency_key=$2").bind(user.into_uuid()).bind(key).bind(&response).bind(now).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(OauthStartResult {
            response,
            replayed: false,
        })
    }

    pub(crate) async fn prepare_oauth_callback(
        &self,
        state: &str,
        code: &str,
        now: DateTime<Utc>,
    ) -> Result<OauthCallbackPreparation, MailStoreError> {
        let state_digest: [u8; 32] = Sha256::digest(state.as_bytes()).into();
        let code_digest: [u8; 32] = Sha256::digest(code.as_bytes()).into();
        let mut tx = self.pool.begin().await?;
        if let Some(row)=sqlx::query("SELECT code_digest,status,response_body FROM mail.oauth_callback_receipts WHERE state_digest=$1 FOR UPDATE").bind(state_digest.as_slice()).fetch_optional(&mut *tx).await? {
            if row.get::<Vec<u8>,_>("code_digest") != code_digest { return Err(MailStoreError::IdempotencyConflict); }
            if row.get::<String,_>("status") == "succeeded" {
                let response=row.get("response_body"); tx.commit().await?;
                return Ok(OauthCallbackPreparation::Replay(CallbackResult{response,replayed:true}));
            }
        }
        let oauth=sqlx::query("SELECT user_id,replacement_connection_id,expected_version,expires_at,consumed_at,verifier_ciphertext,verifier_nonce,key_id FROM mail.oauth_states WHERE state_digest=$1 FOR UPDATE").bind(state_digest.as_slice()).fetch_optional(&mut *tx).await?.ok_or(MailStoreError::InvalidOauthState)?;
        if oauth.get::<DateTime<Utc>, _>("expires_at") < now
            || oauth
                .get::<Option<DateTime<Utc>>, _>("consumed_at")
                .is_some()
        {
            return Err(MailStoreError::InvalidOauthState);
        }
        if oauth.get::<String, _>("key_id") != "parallel-v2-mail" {
            return Err(MailStoreError::InvalidOauthState);
        }
        let verifier = decrypt(
            &oauth.get::<Vec<u8>, _>("verifier_ciphertext"),
            &oauth.get::<Vec<u8>, _>("verifier_nonce"),
            &state_digest,
        )?;
        let verifier =
            String::from_utf8(verifier).map_err(|_| MailStoreError::InvalidOauthState)?;
        if verifier.is_empty() {
            return Err(MailStoreError::InvalidOauthState);
        }
        sqlx::query("INSERT INTO mail.oauth_callback_receipts(state_digest,code_digest,status,created_at) VALUES($1,$2,'processing',$3) ON CONFLICT(state_digest) DO UPDATE SET status='processing',http_status=NULL,redirect_uri=NULL,response_body=NULL,completed_at=NULL")
            .bind(state_digest.as_slice()).bind(code_digest.as_slice()).bind(now)
            .execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(OauthCallbackPreparation::Exchange { verifier })
    }

    pub(crate) async fn complete_oauth(
        &self,
        state: &str,
        code: &str,
        mut tokens: OAuthTokens,
        now: DateTime<Utc>,
    ) -> Result<CallbackResult, MailStoreError> {
        let state_digest: [u8; 32] = Sha256::digest(state.as_bytes()).into();
        let code_digest: [u8; 32] = Sha256::digest(code.as_bytes()).into();
        let mut tx = self.pool.begin().await?;
        let receipt=sqlx::query("SELECT code_digest,status,response_body FROM mail.oauth_callback_receipts WHERE state_digest=$1 FOR UPDATE").bind(state_digest.as_slice()).fetch_optional(&mut *tx).await?.ok_or(MailStoreError::InvalidOauthState)?;
        if receipt.get::<Vec<u8>, _>("code_digest") != code_digest {
            return Err(MailStoreError::IdempotencyConflict);
        }
        if receipt.get::<String, _>("status") == "succeeded" {
            let response = receipt.get("response_body");
            tx.commit().await?;
            return Ok(CallbackResult {
                response,
                replayed: true,
            });
        }
        let oauth=sqlx::query("SELECT user_id,replacement_connection_id,expected_version,expires_at,consumed_at FROM mail.oauth_states WHERE state_digest=$1 FOR UPDATE").bind(state_digest.as_slice()).fetch_optional(&mut *tx).await?.ok_or(MailStoreError::InvalidOauthState)?;
        if oauth.get::<DateTime<Utc>, _>("expires_at") < now
            || oauth
                .get::<Option<DateTime<Utc>>, _>("consumed_at")
                .is_some()
        {
            return Err(MailStoreError::InvalidOauthState);
        }
        let user_id: Uuid = oauth.get("user_id");
        let replacement: Option<Uuid> = oauth.get("replacement_connection_id");
        let connection_id = replacement.unwrap_or_else(Uuid::new_v4);
        if tokens.refresh_token.is_none() {
            if replacement.is_none() {
                return Err(MailStoreError::OAuthProvider);
            }
            if let Some(row)=sqlx::query("SELECT credential_ciphertext,credential_nonce,credential_key_id FROM mail.connections WHERE user_id=$1 AND id=$2 FOR UPDATE")
                .bind(user_id).bind(connection_id).fetch_optional(&mut *tx).await?
                && row.get::<String,_>("credential_key_id") == "parallel-v2-mail"
            {
                let plaintext=decrypt(&row.get::<Vec<u8>,_>("credential_ciphertext"),&row.get::<Vec<u8>,_>("credential_nonce"),connection_id.as_bytes())?;
                if let Ok(existing)=serde_json::from_slice::<EncryptedOAuthCredential>(&plaintext) {
                    tokens.refresh_token=existing.refresh_token;
                }
            }
            if tokens.refresh_token.is_none() {
                return Err(MailStoreError::OAuthProvider);
            }
        }
        let credential = EncryptedOAuthCredential {
            access_token: tokens.access_token,
            refresh_token: tokens.refresh_token,
            expires_at: tokens.expires_at,
        };
        let plaintext =
            serde_json::to_vec(&credential).map_err(|_| MailStoreError::OAuthProvider)?;
        // Connection credentials use a stable, non-secret AAD so a sync worker
        // can decrypt them after the one-time OAuth state has been consumed.
        let (ciphertext, nonce) = encrypt(&plaintext, connection_id.as_bytes())?;
        if replacement.is_some() {
            let expected: Option<i64> = oauth.get("expected_version");
            let row=sqlx::query("UPDATE mail.connections SET state='active',credential_ciphertext=$4,credential_nonce=$5,credential_key_id='parallel-v2-mail',credential_generation=credential_generation+1,sync_generation=sync_generation+1,version=version+1,updated_at=$6 WHERE user_id=$1 AND id=$2 AND version=$3 RETURNING id").bind(user_id).bind(connection_id).bind(expected).bind(&ciphertext).bind(&nonce).bind(now).fetch_optional(&mut *tx).await?;
            row.ok_or(MailStoreError::VersionConflict)?;
            cancel_connection_jobs(&mut tx, user_id, connection_id, now).await?;
        } else {
            sqlx::query("INSERT INTO mail.connections(id,user_id,state,credential_ciphertext,credential_nonce,credential_key_id,created_at,updated_at) VALUES($1,$2,'active',$3,$4,'parallel-v2-mail',$5,$5)").bind(connection_id).bind(user_id).bind(&ciphertext).bind(&nonce).bind(now).execute(&mut *tx).await?;
        }
        let connection = sqlx::query(
            "SELECT version,credential_generation,sync_generation FROM mail.connections WHERE id=$1 AND user_id=$2",
        )
        .bind(connection_id)
        .bind(user_id)
        .fetch_one(&mut *tx)
        .await?;
        sqlx::query("INSERT INTO mail.sync_jobs(id,user_id,connection_id,state,connection_version,credential_generation,sync_generation,created_at,updated_at) VALUES($1,$2,$3,'requested',$4,$5,$6,$7,$7)")
            .bind(Uuid::new_v4()).bind(user_id).bind(connection_id)
            .bind(connection.get::<i64,_>("version"))
            .bind(connection.get::<i64,_>("credential_generation"))
            .bind(connection.get::<i64,_>("sync_generation"))
            .bind(now).execute(&mut *tx).await?;
        sqlx::query("UPDATE mail.oauth_states SET consumed_at=$2 WHERE state_digest=$1")
            .bind(state_digest.as_slice())
            .bind(now)
            .execute(&mut *tx)
            .await?;
        let response = json!({"status":"connected","connection_id":connection_id,"redirect":"/settings/email?status=connected"});
        sqlx::query("UPDATE mail.oauth_callback_receipts SET status='succeeded',http_status=303,redirect_uri='/settings/email?status=connected',connection_id=$3,response_body=$4,completed_at=$5 WHERE state_digest=$1 AND code_digest=$2")
            .bind(state_digest.as_slice()).bind(code_digest.as_slice()).bind(connection_id).bind(&response).bind(now).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(CallbackResult {
            response,
            replayed: false,
        })
    }

    pub(crate) async fn record_oauth_provider_failure(
        &self,
        state: &str,
        code: &str,
        now: DateTime<Utc>,
    ) -> Result<(), sqlx::Error> {
        let state_digest: [u8; 32] = Sha256::digest(state.as_bytes()).into();
        let code_digest: [u8; 32] = Sha256::digest(code.as_bytes()).into();
        sqlx::query("UPDATE mail.oauth_callback_receipts SET status='failed',http_status=502,response_body=jsonb_build_object('error','oauth_provider_failed'),completed_at=$3 WHERE state_digest=$1 AND code_digest=$2 AND status='processing'")
            .bind(state_digest.as_slice()).bind(code_digest.as_slice()).bind(now)
            .execute(&self.pool).await?;
        Ok(())
    }
}

async fn cancel_connection_jobs(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    user_id: Uuid,
    connection_id: Uuid,
    now: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE mail.sync_jobs SET state='cancelled',lease_holder=NULL,lease_expires_at=NULL,next_retry_at=NULL,updated_at=$3 WHERE user_id=$1 AND connection_id=$2 AND state IN ('requested','running','retry_due')")
        .bind(user_id).bind(connection_id).bind(now).execute(&mut **tx).await?;
    Ok(())
}

fn encrypt(plaintext: &[u8], aad: &[u8]) -> Result<(Vec<u8>, Vec<u8>), MailStoreError> {
    let cipher =
        Aes256Gcm::new_from_slice(&[0x53; 32]).map_err(|_| MailStoreError::InvalidOauthState)?;
    let mut nonce = [0_u8; 12];
    OsRng.fill_bytes(&mut nonce);
    let ciphertext = cipher
        .encrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: plaintext,
                aad,
            },
        )
        .map_err(|_| MailStoreError::InvalidOauthState)?;
    Ok((ciphertext, nonce.to_vec()))
}

fn decrypt(ciphertext: &[u8], nonce: &[u8], aad: &[u8]) -> Result<Vec<u8>, MailStoreError> {
    if nonce.len() != 12 {
        return Err(MailStoreError::InvalidOauthState);
    }
    Aes256Gcm::new_from_slice(&[0x53; 32])
        .map_err(|_| MailStoreError::InvalidOauthState)?
        .decrypt(
            Nonce::from_slice(nonce),
            Payload {
                msg: ciphertext,
                aad,
            },
        )
        .map_err(|_| MailStoreError::InvalidOauthState)
}

#[allow(clippy::too_many_arguments)]
async fn claim_mail_receipt(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    user: UserId,
    scope: &str,
    key: &str,
    command: &str,
    target: Option<Uuid>,
    hash: [u8; 32],
    now: DateTime<Utc>,
) -> Result<Option<Value>, MailStoreError> {
    let inserted=sqlx::query("INSERT INTO mail.command_receipts(user_id,command_scope,idempotency_key,command_name,target_id,request_hash,status,created_at) VALUES($1,$2,$3,$4,$5,$6,'processing',$7) ON CONFLICT DO NOTHING").bind(user.into_uuid()).bind(scope).bind(key).bind(command).bind(target).bind(hash.as_slice()).bind(now).execute(&mut **tx).await?;
    if inserted.rows_affected() == 1 {
        return Ok(None);
    }
    let row=sqlx::query("SELECT request_hash,status,response_body FROM mail.command_receipts WHERE user_id=$1 AND command_scope=$2 AND idempotency_key=$3").bind(user.into_uuid()).bind(scope).bind(key).fetch_one(&mut **tx).await?;
    if row.get::<Vec<u8>, _>("request_hash") != hash {
        return Err(MailStoreError::IdempotencyConflict);
    }
    let status: String = row.get("status");
    if status == "rejected" {
        let response: Value = row.try_get("response_body")?;
        return Err(match response.get("error").and_then(Value::as_str) {
            Some("not_found") => MailStoreError::NotFound,
            _ => MailStoreError::VersionConflict,
        });
    }
    if status != "succeeded" {
        return Err(MailStoreError::VersionConflict);
    }
    Ok(row.try_get("response_body")?)
}

#[allow(clippy::too_many_arguments)]
async fn reject_mail_receipt(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    user: UserId,
    scope: &str,
    key: &str,
    code: &str,
    http: i16,
    now: DateTime<Utc>,
) -> Result<(), MailStoreError> {
    sqlx::query("UPDATE mail.command_receipts SET status='rejected',http_status=$4,response_body=$5,completed_at=$6 WHERE user_id=$1 AND command_scope=$2 AND idempotency_key=$3")
        .bind(user.into_uuid()).bind(scope).bind(key).bind(http)
        .bind(json!({"error":code})).bind(now).execute(&mut **tx).await?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn finish_mail_receipt(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    user: UserId,
    scope: &str,
    key: &str,
    http: i16,
    response: &Value,
    aggregate: Uuid,
    version: i64,
    now: DateTime<Utc>,
) -> Result<(), MailStoreError> {
    sqlx::query("UPDATE mail.command_receipts SET status='succeeded',http_status=$4,response_body=$5,aggregate_id=$6,aggregate_version=$7,completed_at=$8 WHERE user_id=$1 AND command_scope=$2 AND idempotency_key=$3").bind(user.into_uuid()).bind(scope).bind(key).bind(http).bind(response).bind(aggregate).bind(version).bind(now).execute(&mut **tx).await?;
    Ok(())
}
fn row_view(row: sqlx::postgres::PgRow) -> Result<ConnectionView, sqlx::Error> {
    let state = match row.get::<String, _>("state").as_str() {
        "pending" => ConnectionState::Pending,
        "active" => ConnectionState::Active,
        "needs_reauth" => ConnectionState::NeedsReauth,
        "disconnected" => ConnectionState::Disconnected,
        _ => return Err(sqlx::Error::Protocol("invalid mail state".into())),
    };
    Ok(ConnectionView {
        id: GmailConnectionId::new(row.get("id")),
        state,
        version: ConnectionVersion::new(
            u64::try_from(row.get::<i64, _>("version")).unwrap_or_default(),
        )
        .map_err(|_| sqlx::Error::Protocol("invalid mail version".into()))?,
        credential_generation: u64::try_from(row.get::<i64, _>("credential_generation"))
            .unwrap_or_default(),
        sync_generation: u64::try_from(row.get::<i64, _>("sync_generation")).unwrap_or_default(),
    })
}
