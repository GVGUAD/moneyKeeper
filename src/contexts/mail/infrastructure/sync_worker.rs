//! One bounded, generation-fenced Gmail synchronization invocation.

use std::time::Duration;

use aes_gcm::{
    Aes256Gcm, KeyInit, Nonce,
    aead::{Aead, Payload},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Utc};
use rand::{RngCore, rngs::OsRng};
use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Postgres, Row, Transaction};
use uuid::Uuid;

use crate::{
    contexts::mail::application::ports::{GmailMessage, GmailOAuth, GmailSource},
    domain::email::RawEmail,
};

use super::oauth::OAuthProviderError;
use super::parsers::ParserRegistry;
use super::repository::EncryptedOAuthCredential;

const CREDENTIAL_KEY: [u8; 32] = [0x53; 32];
const CREDENTIAL_KEY_ID: &str = "parallel-v2-mail";

#[derive(Debug, thiserror::Error)]
pub(crate) enum SyncError {
    #[error("mail sync persistence failed")]
    Database(#[from] sqlx::Error),
    #[error("mail credential could not be decrypted")]
    Credential,
    #[error("gmail request failed")]
    Provider,
    #[error("mail sync configuration is invalid")]
    Configuration,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct SyncReport {
    pub claimed: bool,
    pub messages_recorded: u32,
    pub evidence_recorded: u32,
    pub completed: bool,
    pub retry_scheduled: bool,
    pub fenced: bool,
}

#[derive(Clone)]
pub(crate) struct MailSyncWorker<S, O> {
    pool: PgPool,
    source: S,
    oauth: O,
    holder: String,
    lease_ttl: Duration,
}

impl<S, O> MailSyncWorker<S, O>
where
    S: GmailSource,
    O: GmailOAuth,
{
    pub(crate) fn new(
        pool: PgPool,
        source: S,
        oauth: O,
        holder: impl Into<String>,
        lease_ttl: Duration,
    ) -> Result<Self, SyncError> {
        let holder = holder.into();
        if holder.trim() != holder || holder.is_empty() || holder.len() > 200 || lease_ttl.is_zero()
        {
            return Err(SyncError::Configuration);
        }
        Ok(Self {
            pool,
            source,
            oauth,
            holder,
            lease_ttl,
        })
    }

    pub(crate) async fn run_once(&self) -> Result<SyncReport, SyncError> {
        let Some(claim) = self.claim().await? else {
            return Ok(SyncReport::default());
        };
        let token = match decrypt_credential(
            &claim.credential_ciphertext,
            &claim.credential_nonce,
            claim.connection_id,
        ) {
            Ok(token) => token,
            Err(_) => {
                let retry_scheduled = self.record_failure(&claim).await?;
                return Ok(SyncReport {
                    claimed: true,
                    retry_scheduled,
                    fenced: !retry_scheduled,
                    ..SyncReport::default()
                });
            }
        };
        let mut credential = match serde_json::from_slice::<EncryptedOAuthCredential>(&token) {
            Ok(credential) => credential,
            Err(_) => {
                let retry_scheduled = self.record_failure(&claim).await?;
                return Ok(SyncReport {
                    claimed: true,
                    retry_scheduled,
                    fenced: !retry_scheduled,
                    ..SyncReport::default()
                });
            }
        };
        if credential.expires_at <= Utc::now() {
            let Some(refresh_token) = credential.refresh_token.as_deref() else {
                let changed = self.mark_needs_reauth(&claim).await?;
                return Ok(SyncReport {
                    claimed: true,
                    fenced: !changed,
                    ..SyncReport::default()
                });
            };
            let refreshed = match self.oauth.refresh(refresh_token).await {
                Ok(refreshed) => refreshed,
                Err(error)
                    if matches!(
                        error.downcast_ref::<OAuthProviderError>(),
                        Some(OAuthProviderError::InvalidCredentials)
                    ) =>
                {
                    let changed = self.mark_needs_reauth(&claim).await?;
                    return Ok(SyncReport {
                        claimed: true,
                        fenced: !changed,
                        ..SyncReport::default()
                    });
                }
                Err(_) => {
                    let retry_scheduled = self.record_failure(&claim).await?;
                    return Ok(SyncReport {
                        claimed: true,
                        retry_scheduled,
                        fenced: !retry_scheduled,
                        ..SyncReport::default()
                    });
                }
            };
            credential = EncryptedOAuthCredential {
                access_token: refreshed.access_token,
                refresh_token: refreshed
                    .refresh_token
                    .or_else(|| credential.refresh_token.take()),
                expires_at: refreshed.expires_at,
            };
            if !self
                .persist_refreshed_credential(&claim, &credential)
                .await?
            {
                return Ok(SyncReport {
                    claimed: true,
                    fenced: true,
                    ..SyncReport::default()
                });
            }
        }
        let page = match self
            .source
            .fetch_page(&credential.access_token, claim.cursor.as_deref())
            .await
        {
            Ok(page) => page,
            Err(error) if gmail_requires_reauth(&error) => {
                let changed = self.mark_needs_reauth(&claim).await?;
                return Ok(SyncReport {
                    claimed: true,
                    fenced: !changed,
                    ..SyncReport::default()
                });
            }
            Err(_) => {
                let retry_scheduled = self.record_failure(&claim).await?;
                return Ok(SyncReport {
                    claimed: true,
                    retry_scheduled,
                    fenced: !retry_scheduled,
                    ..SyncReport::default()
                });
            }
        };
        self.commit_page(&claim, page.messages, page.next_cursor)
            .await
    }

    async fn persist_refreshed_credential(
        &self,
        claim: &SyncClaim,
        credential: &EncryptedOAuthCredential,
    ) -> Result<bool, SyncError> {
        let plaintext = serde_json::to_vec(credential).map_err(|_| SyncError::Credential)?;
        let (ciphertext, nonce) = encrypt_credential(&plaintext, claim.connection_id)?;
        let updated = sqlx::query(
            r#"
            UPDATE mail.connections c SET credential_ciphertext=$7,credential_nonce=$8,
                updated_at=clock_timestamp()
            FROM mail.sync_jobs j
            WHERE c.id=$1 AND c.user_id=$2 AND c.state='active'
              AND c.credential_generation=$3 AND c.sync_generation=$4
              AND j.id=$5 AND j.user_id=$2 AND j.connection_id=$1
              AND j.lease_holder=$6 AND j.lease_token=$9
              AND j.lease_expires_at>clock_timestamp() AND j.state='running'
            "#,
        )
        .bind(claim.connection_id)
        .bind(claim.user_id)
        .bind(claim.credential_generation)
        .bind(claim.sync_generation)
        .bind(claim.job_id)
        .bind(&self.holder)
        .bind(ciphertext)
        .bind(nonce)
        .bind(claim.lease_token)
        .execute(&self.pool)
        .await?;
        Ok(updated.rows_affected() == 1)
    }

    async fn claim(&self) -> Result<Option<SyncClaim>, SyncError> {
        let ttl_millis =
            i64::try_from(self.lease_ttl.as_millis()).map_err(|_| SyncError::Configuration)?;
        let row = sqlx::query(
            r#"
            WITH candidate AS (
                SELECT j.id,j.user_id,j.connection_id,j.cursor,j.credential_generation,
                       j.sync_generation,c.credential_ciphertext,c.credential_nonce,c.credential_key_id
                FROM mail.sync_jobs j
                JOIN mail.connections c ON c.id=j.connection_id AND c.user_id=j.user_id
                WHERE j.state IN ('requested','retry_due','running')
                  AND (j.next_retry_at IS NULL OR j.next_retry_at<=clock_timestamp())
                  AND (j.lease_expires_at IS NULL OR j.lease_expires_at<=clock_timestamp())
                  AND c.state='active'
                  AND c.credential_generation=j.credential_generation
                  AND c.sync_generation=j.sync_generation
                ORDER BY j.created_at,j.id
                FOR UPDATE OF j SKIP LOCKED
                LIMIT 1
            )
            UPDATE mail.sync_jobs j SET
                state='running',lease_holder=$1,
                lease_expires_at=clock_timestamp()+($2::bigint*interval '1 millisecond'),
                lease_token=j.lease_token+1,attempts=j.attempts+1,updated_at=clock_timestamp()
            FROM candidate c WHERE j.id=c.id AND j.user_id=c.user_id
            RETURNING j.id,j.user_id,j.connection_id,j.cursor,j.credential_generation,
                      j.sync_generation,j.lease_token,c.credential_ciphertext,
                      c.credential_nonce,c.credential_key_id
            "#,
        )
        .bind(&self.holder)
        .bind(ttl_millis)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|row| {
            let key_id: String = row.get("credential_key_id");
            if key_id != CREDENTIAL_KEY_ID {
                return Err(SyncError::Credential);
            }
            Ok(SyncClaim {
                job_id: row.get("id"),
                user_id: row.get("user_id"),
                connection_id: row.get("connection_id"),
                cursor: row.get("cursor"),
                credential_generation: row.get("credential_generation"),
                sync_generation: row.get("sync_generation"),
                lease_token: row.get("lease_token"),
                credential_ciphertext: row.get("credential_ciphertext"),
                credential_nonce: row.get("credential_nonce"),
            })
        })
        .transpose()
    }

    async fn record_failure(&self, claim: &SyncClaim) -> Result<bool, SyncError> {
        let mut transaction = self.pool.begin().await?;
        let row = sqlx::query(
            r#"
            UPDATE mail.sync_jobs SET state=CASE WHEN attempts>=10 THEN 'failed' ELSE 'retry_due' END,
                next_retry_at=CASE WHEN attempts>=10 THEN NULL ELSE clock_timestamp()+
                    (LEAST(3600,CAST(power(2,LEAST(attempts,11)) AS BIGINT))*interval '1 second') END,
                last_error='gmail request failed; provider details redacted',
                lease_holder=NULL,lease_expires_at=NULL,updated_at=clock_timestamp()
            WHERE id=$1 AND user_id=$2 AND lease_holder=$3 AND lease_token=$4
              AND lease_expires_at>clock_timestamp() AND state='running'
            RETURNING user_id,state,cursor
            "#,
        )
        .bind(claim.job_id)
        .bind(claim.user_id)
        .bind(&self.holder)
        .bind(claim.lease_token)
        .fetch_optional(&mut *transaction)
        .await?;
        let Some(row) = row else {
            transaction.rollback().await?;
            return Ok(false);
        };
        let state: String = row.get("state");
        sqlx::query("INSERT INTO mail.fetch_attempts(id,user_id,job_id,state,page_cursor,error_code,started_at,finished_at) VALUES($1,$2,$3,$4,$5,'provider_request_failed',$6,$6)")
            .bind(Uuid::new_v4()).bind(row.get::<Uuid,_>("user_id")).bind(claim.job_id)
            .bind(if state == "failed" { "failed" } else { "retry_due" })
            .bind(row.get::<Option<String>,_>("cursor")).bind(Utc::now())
            .execute(&mut *transaction).await?;
        transaction.commit().await?;
        Ok(true)
    }

    async fn mark_needs_reauth(&self, claim: &SyncClaim) -> Result<bool, SyncError> {
        let mut transaction = self.pool.begin().await?;
        let changed = sqlx::query(
            r#"
            UPDATE mail.connections SET state='needs_reauth',version=version+1,
                sync_generation=sync_generation+1,updated_at=clock_timestamp()
            WHERE id=$1 AND user_id=$2 AND state='active'
              AND credential_generation=$3 AND sync_generation=$4
              AND EXISTS(
                  SELECT 1 FROM mail.sync_jobs
                  WHERE id=$5 AND user_id=$2 AND connection_id=$1
                    AND lease_holder=$6 AND lease_token=$7
                    AND lease_expires_at>clock_timestamp() AND state='running'
              )
            "#,
        )
        .bind(claim.connection_id)
        .bind(claim.user_id)
        .bind(claim.credential_generation)
        .bind(claim.sync_generation)
        .bind(claim.job_id)
        .bind(&self.holder)
        .bind(claim.lease_token)
        .execute(&mut *transaction)
        .await?;
        if changed.rows_affected() == 0 {
            transaction.rollback().await?;
            return Ok(false);
        }
        let now = Utc::now();
        sqlx::query("UPDATE mail.sync_jobs SET state='failed',last_error='gmail credentials require reauthorization',next_retry_at=NULL,lease_holder=NULL,lease_expires_at=NULL,updated_at=$5 WHERE id=$1 AND user_id=$2 AND lease_holder=$3 AND lease_token=$4")
            .bind(claim.job_id).bind(claim.user_id).bind(&self.holder).bind(claim.lease_token).bind(now)
            .execute(&mut *transaction).await?;
        sqlx::query("INSERT INTO mail.fetch_attempts(id,user_id,job_id,state,page_cursor,error_code,started_at,finished_at) VALUES($1,$2,$3,'failed',$4,'needs_reauth',$5,$5)")
            .bind(Uuid::new_v4()).bind(claim.user_id).bind(claim.job_id).bind(claim.cursor.as_deref()).bind(now)
            .execute(&mut *transaction).await?;
        transaction.commit().await?;
        Ok(true)
    }

    async fn commit_page(
        &self,
        claim: &SyncClaim,
        messages: Vec<GmailMessage>,
        next_cursor: Option<String>,
    ) -> Result<SyncReport, SyncError> {
        let mut transaction = self.pool.begin().await?;
        if !claim_is_current(&mut transaction, claim, &self.holder).await? {
            transaction.rollback().await?;
            return Ok(SyncReport {
                claimed: true,
                fenced: true,
                ..SyncReport::default()
            });
        }
        let attempt_id = Uuid::new_v4();
        let started_at = Utc::now();
        let mut recorded = 0_u32;
        let mut evidence = 0_u32;
        for message in messages {
            let result = record_message(&mut transaction, claim, message).await?;
            recorded += u32::from(result.recorded);
            evidence += u32::from(result.evidence_recorded);
        }
        let now = Utc::now();
        sqlx::query(
            "INSERT INTO mail.fetch_attempts(id,user_id,job_id,state,page_cursor,started_at,finished_at) VALUES($1,$2,$3,'succeeded',$4,$5,$6)",
        )
        .bind(attempt_id)
        .bind(claim.user_id)
        .bind(claim.job_id)
        .bind(claim.cursor.as_deref())
        .bind(started_at)
        .bind(now)
        .execute(&mut *transaction)
        .await?;
        let completed = next_cursor.is_none();
        let state = if completed { "completed" } else { "requested" };
        let updated = sqlx::query(
            r#"
            UPDATE mail.sync_jobs SET state=$5,cursor=$6,next_retry_at=NULL,last_error=NULL,
                lease_holder=NULL,lease_expires_at=NULL,updated_at=$7
            WHERE id=$1 AND user_id=$2 AND lease_holder=$3 AND lease_token=$4
              AND credential_generation=$8 AND sync_generation=$9
              AND lease_expires_at>clock_timestamp() AND state='running'
            "#,
        )
        .bind(claim.job_id)
        .bind(claim.user_id)
        .bind(&self.holder)
        .bind(claim.lease_token)
        .bind(state)
        .bind(next_cursor)
        .bind(now)
        .bind(claim.credential_generation)
        .bind(claim.sync_generation)
        .execute(&mut *transaction)
        .await?;
        if updated.rows_affected() != 1 {
            transaction.rollback().await?;
            return Ok(SyncReport {
                claimed: true,
                fenced: true,
                ..SyncReport::default()
            });
        }
        transaction.commit().await?;
        Ok(SyncReport {
            claimed: true,
            messages_recorded: recorded,
            evidence_recorded: evidence,
            completed,
            ..SyncReport::default()
        })
    }
}

struct SyncClaim {
    job_id: Uuid,
    user_id: Uuid,
    connection_id: Uuid,
    cursor: Option<String>,
    credential_generation: i64,
    sync_generation: i64,
    lease_token: i64,
    credential_ciphertext: Vec<u8>,
    credential_nonce: Vec<u8>,
}

#[derive(Clone, Copy)]
struct RecordResult {
    recorded: bool,
    evidence_recorded: bool,
}

async fn claim_is_current(
    transaction: &mut Transaction<'_, Postgres>,
    claim: &SyncClaim,
    holder: &str,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar(
        r#"
        SELECT EXISTS(
            SELECT 1 FROM mail.sync_jobs j
            JOIN mail.connections c ON c.id=j.connection_id AND c.user_id=j.user_id
            WHERE j.id=$1 AND j.user_id=$2 AND j.lease_holder=$3 AND j.lease_token=$4
              AND j.lease_expires_at>clock_timestamp() AND j.state='running'
              AND j.credential_generation=$5 AND j.sync_generation=$6
              AND c.state='active' AND c.credential_generation=$5 AND c.sync_generation=$6
        )
        "#,
    )
    .bind(claim.job_id)
    .bind(claim.user_id)
    .bind(holder)
    .bind(claim.lease_token)
    .bind(claim.credential_generation)
    .bind(claim.sync_generation)
    .fetch_one(&mut **transaction)
    .await
}

async fn record_message(
    transaction: &mut Transaction<'_, Postgres>,
    claim: &SyncClaim,
    message: GmailMessage,
) -> Result<RecordResult, SyncError> {
    let raw = RawEmail {
        provider_message_id: message.provider_id.clone(),
        rfc_message_id: None,
        from: message.from,
        subject: message.subject,
        authentication_results: Vec::new(),
        received_at: message.received_at,
        body_text: message.body_text,
        body_html: message.body_html,
    };
    let plaintext = serde_json::to_vec(&SerializableEmail::from(&raw))
        .expect("normalized mail payload serializes");
    let digest: [u8; 32] = Sha256::digest(&plaintext).into();
    let message_id = Uuid::new_v4();
    let (ciphertext, nonce) = encrypt_payload(&plaintext, message_id)?;
    let existing: Option<Uuid> = sqlx::query_scalar("SELECT id FROM mail.source_messages WHERE connection_id=$1 AND provider_message_id=$2 AND payload_digest=$3")
        .bind(claim.connection_id).bind(&raw.provider_message_id).bind(digest.as_slice())
        .fetch_optional(&mut **transaction).await?;
    if existing.is_some() {
        return Ok(RecordResult {
            recorded: false,
            evidence_recorded: false,
        });
    }
    let revision: i64 = sqlx::query_scalar("SELECT COALESCE(MAX(revision),0)+1 FROM mail.source_messages WHERE connection_id=$1 AND provider_message_id=$2")
        .bind(claim.connection_id).bind(&raw.provider_message_id)
        .fetch_one(&mut **transaction).await?;
    let inserted = sqlx::query(
        r#"
        INSERT INTO mail.source_messages
            (id,user_id,connection_id,provider_message_id,revision,payload_digest,
             payload_ciphertext,payload_nonce,key_id,received_at,recorded_at)
        VALUES($1,$2,$3,$4,$11,$5,$6,$7,$8,$9,$10)
        ON CONFLICT(connection_id,provider_message_id,payload_digest) DO NOTHING
        "#,
    )
    .bind(message_id)
    .bind(claim.user_id)
    .bind(claim.connection_id)
    .bind(&raw.provider_message_id)
    .bind(digest.as_slice())
    .bind(ciphertext)
    .bind(nonce)
    .bind(CREDENTIAL_KEY_ID)
    .bind(raw.received_at)
    .bind(Utc::now())
    .bind(revision)
    .execute(&mut **transaction)
    .await?;
    if inserted.rows_affected() == 0 {
        return Ok(RecordResult {
            recorded: false,
            evidence_recorded: false,
        });
    }

    let registry = ParserRegistry::default_set();
    match registry.parse(&raw) {
        Ok(Some(parsed)) => {
            let evidence_id = Uuid::new_v4();
            let provenance = serde_json::json!({
                "source_message_id": message_id,
                "payload_digest": URL_SAFE_NO_PAD.encode(digest),
                "parser": parsed.parser_name,
                "parser_version": parsed.parser_version
            });
            sqlx::query("INSERT INTO mail.parse_attempts(id,user_id,message_id,parser_name,parser_version,state,recorded_at) VALUES($1,$2,$3,$4,$5,'parsed',$6)")
                .bind(Uuid::new_v4()).bind(claim.user_id).bind(message_id)
                .bind(parsed.parser_name).bind(parsed.parser_version).bind(Utc::now())
                .execute(&mut **transaction).await?;
            sqlx::query("INSERT INTO mail.receipt_evidence(id,user_id,message_id,parser_name,parser_version,evidence_kind,merchant,amount,currency,charged_at,provenance,recorded_at) VALUES($1,$2,$3,$4,$5,'renewal',$6,$7,$8,$9,$10,$11)")
                .bind(evidence_id).bind(claim.user_id).bind(message_id)
                .bind(parsed.parser_name).bind(parsed.parser_version)
                .bind(&parsed.receipt.merchant_key).bind(parsed.receipt.amount)
                .bind(&parsed.receipt.currency).bind(parsed.receipt.charged_at)
                .bind(&provenance).bind(Utc::now()).execute(&mut **transaction).await?;
            let payload = serde_json::json!({
                "event": crate::contexts::mail::public::ReceiptEvidenceRecordedV1 {
                    evidence_id: crate::contexts::mail::public::ReceiptEvidenceId::new(evidence_id),
                    user_id: crate::shared_kernel::UserId::new(claim.user_id),
                    source_message_id: crate::contexts::mail::domain::SourceMessageId::new(message_id),
                    merchant: parsed.receipt.merchant_key,
                    kind: crate::contexts::mail::public::ReceiptEvidenceKind::Renewal,
                    money: Some(crate::shared_kernel::Money::new(
                        parsed.receipt.amount,
                        crate::shared_kernel::CurrencyCode::new(&parsed.receipt.currency)
                            .map_err(|_| SyncError::Provider)?,
                        parsed.receipt.amount.scale(),
                    ).map_err(|_| SyncError::Provider)?),
                    charged_at: Some(parsed.receipt.charged_at),
                    parser_name: parsed.parser_name.to_owned(),
                    parser_version: u32::try_from(parsed.parser_version)
                        .map_err(|_| SyncError::Provider)?,
                    provenance_digest: digest,
                    recorded_at: Utc::now(),
                }
            })["event"].clone();
            sqlx::query("INSERT INTO integration.outbox_messages(message_id,event_id,message_schema_version,context_name,aggregate_id,aggregate_version,event_type,user_id,occurred_at,correlation_id,payload) VALUES($1,$2,1,'mail',$3,1,'mail.receipt-evidence-recorded.v1',$4,$5,$6,$7)")
                .bind(Uuid::new_v4()).bind(Uuid::new_v4()).bind(evidence_id.to_string())
                .bind(claim.user_id).bind(Utc::now()).bind(evidence_id).bind(payload)
                .execute(&mut **transaction).await?;
            Ok(RecordResult {
                recorded: true,
                evidence_recorded: true,
            })
        }
        Ok(None) => {
            sqlx::query("INSERT INTO mail.parse_attempts(id,user_id,message_id,parser_name,parser_version,state,recorded_at) VALUES($1,$2,$3,'registry',1,'unsupported',$4)")
                .bind(Uuid::new_v4()).bind(claim.user_id).bind(message_id).bind(Utc::now())
                .execute(&mut **transaction).await?;
            Ok(RecordResult {
                recorded: true,
                evidence_recorded: false,
            })
        }
        Err(_) => {
            sqlx::query("INSERT INTO mail.parse_attempts(id,user_id,message_id,parser_name,parser_version,state,error_code,recorded_at) VALUES($1,$2,$3,'registry',1,'malformed','parse_failed',$4)")
                .bind(Uuid::new_v4()).bind(claim.user_id).bind(message_id).bind(Utc::now())
                .execute(&mut **transaction).await?;
            Ok(RecordResult {
                recorded: true,
                evidence_recorded: false,
            })
        }
    }
}

fn decrypt_credential(
    ciphertext: &[u8],
    nonce: &[u8],
    connection_id: Uuid,
) -> Result<Vec<u8>, SyncError> {
    if nonce.len() != 12 {
        return Err(SyncError::Credential);
    }
    Aes256Gcm::new_from_slice(&CREDENTIAL_KEY)
        .map_err(|_| SyncError::Credential)?
        .decrypt(
            Nonce::from_slice(nonce),
            Payload {
                msg: ciphertext,
                aad: connection_id.as_bytes(),
            },
        )
        .map_err(|_| SyncError::Credential)
}

fn gmail_requires_reauth(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<reqwest::Error>()
        .and_then(reqwest::Error::status)
        .is_some_and(|status| {
            status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN
        })
}

fn encrypt_credential(
    plaintext: &[u8],
    connection_id: Uuid,
) -> Result<(Vec<u8>, Vec<u8>), SyncError> {
    let cipher = Aes256Gcm::new_from_slice(&CREDENTIAL_KEY).map_err(|_| SyncError::Credential)?;
    let mut nonce = [0_u8; 12];
    OsRng.fill_bytes(&mut nonce);
    let ciphertext = cipher
        .encrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: plaintext,
                aad: connection_id.as_bytes(),
            },
        )
        .map_err(|_| SyncError::Credential)?;
    Ok((ciphertext, nonce.to_vec()))
}

fn encrypt_payload(plaintext: &[u8], message_id: Uuid) -> Result<(Vec<u8>, Vec<u8>), SyncError> {
    let cipher = Aes256Gcm::new_from_slice(&CREDENTIAL_KEY).map_err(|_| SyncError::Credential)?;
    let mut nonce = [0_u8; 12];
    OsRng.fill_bytes(&mut nonce);
    let ciphertext = cipher
        .encrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: plaintext,
                aad: message_id.as_bytes(),
            },
        )
        .map_err(|_| SyncError::Credential)?;
    Ok((ciphertext, nonce.to_vec()))
}

#[derive(Serialize)]
struct SerializableEmail<'a> {
    provider_message_id: &'a str,
    rfc_message_id: &'a Option<String>,
    from: &'a str,
    subject: &'a str,
    authentication_results: &'a [String],
    received_at: DateTime<Utc>,
    body_text: &'a Option<String>,
    body_html: &'a Option<String>,
}

impl<'a> From<&'a RawEmail> for SerializableEmail<'a> {
    fn from(email: &'a RawEmail) -> Self {
        Self {
            provider_message_id: &email.provider_message_id,
            rfc_message_id: &email.rfc_message_id,
            from: &email.from,
            subject: &email.subject,
            authentication_results: &email.authentication_results,
            received_at: email.received_at,
            body_text: &email.body_text,
            body_html: &email.body_html,
        }
    }
}
