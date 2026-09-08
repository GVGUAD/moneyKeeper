//! Bounded, fenced Banking worker steps. Claims are committed by the
//! repository before this layer decrypts credentials or performs provider I/O.

use chrono::{DateTime, Duration, Utc};
use tracing::Instrument as _;

use super::{
    BankingFacade, CredentialBinding, ProviderCredential, ProviderCurrency, ProviderCurrencyMap,
    ProviderFailure, ProviderFailureClass, WebhookProvisioning,
};
use crate::contexts::banking::domain::BankingError;

const LEASE_SECONDS: i64 = 30;
const MAX_ATTEMPTS: i32 = 10;
const MIN_RETRY_SECONDS: i64 = 61;
const MAX_RETRY_SECONDS: i64 = 3_600;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BankingWorkerStepReport {
    pub claimed: bool,
    pub records: u32,
    pub retry_scheduled: bool,
    pub fenced: bool,
}

impl BankingFacade {
    pub async fn run_validation_once(
        &self,
        holder: &str,
        now: DateTime<Utc>,
    ) -> Result<BankingWorkerStepReport, BankingError> {
        let Some(work) = self
            .workers
            .claim_validation(holder, now, LEASE_SECONDS)
            .await?
        else {
            return Ok(BankingWorkerStepReport::default());
        };
        let item_span = tracing::info_span!(
            "worker.item",
            operation = "banking.validation",
            connection_id = %work.connection_id,
        );
        log_claimed(&item_span);
        let binding = CredentialBinding::new(
            work.user_id,
            work.connection_id.into_uuid(),
            &work.provider,
            work.generation,
            if work.replacement {
                "pending"
            } else {
                "active"
            },
        )?;
        let credential = match self.cipher.decrypt(&work.envelope, &binding) {
            Ok(credential) => credential,
            Err(_) => {
                let completed_at = completion_time(now);
                let completed = self
                    .workers
                    .complete_validation_failure(
                        &work,
                        ProviderFailureClass::Terminal,
                        None,
                        completed_at,
                    )
                    .await?;
                return Ok(BankingWorkerStepReport {
                    claimed: true,
                    fenced: !completed,
                    ..BankingWorkerStepReport::default()
                });
            }
        };
        let body = match self
            .provider
            .client_info(&credential)
            .instrument(item_span)
            .await
        {
            Ok(body) => body,
            Err(failure) => {
                return self.finish_validation_failure(&work, failure, now).await;
            }
        };
        let currencies = self.currency_map().await?;
        let snapshot = match self.normalizer.client_info(&body, &currencies) {
            Ok(snapshot) => snapshot,
            Err(_) => {
                let completed_at = completion_time(now);
                let completed = self
                    .workers
                    .complete_validation_failure(
                        &work,
                        ProviderFailureClass::Terminal,
                        None,
                        completed_at,
                    )
                    .await?;
                return Ok(BankingWorkerStepReport {
                    claimed: true,
                    fenced: !completed,
                    ..BankingWorkerStepReport::default()
                });
            }
        };
        let active_envelope = if work.replacement {
            Some(self.cipher.encrypt(
                &credential,
                &CredentialBinding::new(
                    work.user_id,
                    work.connection_id.into_uuid(),
                    &work.provider,
                    work.generation,
                    "active",
                )?,
            )?)
        } else {
            None
        };
        let webhook = if work.webhook_configured {
            None
        } else {
            Some(self.provision_webhook(&work, 1)?)
        };
        let completed_at = completion_time(now);
        let completed = self
            .workers
            .complete_validation_success(
                &work,
                active_envelope.as_ref(),
                webhook.as_ref(),
                &snapshot.resources,
                completed_at,
            )
            .await?;
        Ok(BankingWorkerStepReport {
            claimed: true,
            records: if completed {
                snapshot.resources.len() as u32
            } else {
                0
            },
            fenced: !completed,
            ..BankingWorkerStepReport::default()
        })
    }

    pub async fn run_webhook_registration_once(
        &self,
        holder: &str,
        callback_base: &str,
        now: DateTime<Utc>,
    ) -> Result<BankingWorkerStepReport, BankingError> {
        let Some(work) = self
            .workers
            .claim_webhook_registration(holder, now, LEASE_SECONDS)
            .await?
        else {
            return Ok(BankingWorkerStepReport::default());
        };
        let item_span = tracing::info_span!(
            "worker.item",
            operation = "banking.webhook_registration",
            connection_id = %work.connection_id,
        );
        log_claimed(&item_span);
        let provider_token = self.cipher.decrypt(
            &work.provider_envelope,
            &CredentialBinding::new(
                work.user_id,
                work.connection_id.into_uuid(),
                &work.provider,
                work.credential_generation,
                "active",
            )?,
        );
        let webhook_token = self.cipher.decrypt(
            &work.webhook_envelope,
            &CredentialBinding::new(
                work.user_id,
                work.connection_id.into_uuid(),
                &work.provider,
                work.webhook_version,
                "webhook",
            )?,
        );
        let failure = match (provider_token, webhook_token) {
            (Ok(provider_token), Ok(webhook_token)) => {
                let callback = format!(
                    "{}webhooks/monobank/{}",
                    callback_base,
                    webhook_token.expose()
                );
                self.provider
                    .register_webhook(&provider_token, &callback)
                    .instrument(item_span)
                    .await
                    .err()
            }
            _ => Some(ProviderFailure::Classified {
                class: ProviderFailureClass::Terminal,
            }),
        };
        let completed_at = completion_time(now);
        let (class, next_retry_at) = failure.as_ref().map_or((None, None), |failure| {
            let class = failure.class();
            let next = retry_at(work.attempts, failure, completed_at);
            (Some(class), next)
        });
        let completed = self
            .workers
            .complete_claimed_webhook_registration(&work, class, next_retry_at, completed_at)
            .await?;
        Ok(BankingWorkerStepReport {
            claimed: true,
            records: u32::from(completed && failure.is_none()),
            retry_scheduled: completed && next_retry_at.is_some(),
            fenced: !completed,
        })
    }

    pub async fn run_webhook_receipt_once(
        &self,
        holder: &str,
        now: DateTime<Utc>,
    ) -> Result<BankingWorkerStepReport, BankingError> {
        let Some(work) = self
            .workers
            .claim_webhook_receipt(holder, now, LEASE_SECONDS)
            .await?
        else {
            return Ok(BankingWorkerStepReport::default());
        };
        let item_span = tracing::info_span!(
            "worker.item",
            operation = "banking.webhook_receipt",
            connection_id = %work.connection_id,
            webhook_receipt_id = %work.receipt_id,
        );
        log_claimed(&item_span);
        let binding = CredentialBinding::new(
            work.user_id,
            work.connection_id.into_uuid(),
            &work.provider,
            work.binding_generation,
            format!("provenance:{}", work.receipt_id),
        )?;
        let normalized = match self.cipher.decrypt_payload(&work.envelope, &binding) {
            Ok(payload) => {
                let external_id = serde_json::from_slice::<serde_json::Value>(&payload)
                    .ok()
                    .and_then(|value| value.pointer("/data/account")?.as_str().map(str::to_owned));
                match external_id {
                    Some(external_id) => match self
                        .workers
                        .webhook_resource_currency(&work, &external_id)
                        .await
                    {
                        Ok((currency, _)) => {
                            let currencies = self.currency_map().await?;
                            self.normalizer
                                .webhook(&payload, &currency, &currencies)
                                .map_err(|_| "invalid_payload")
                        }
                        Err(_) => Err("unknown_resource"),
                    },
                    None => Err("invalid_payload"),
                }
            }
            Err(_) => Err("payload_unavailable"),
        };
        let was_valid = normalized.is_ok();
        let completed_at = completion_time(now);
        let completed = self
            .workers
            .complete_webhook_receipt(&work, normalized, completed_at)
            .await?;
        Ok(BankingWorkerStepReport {
            claimed: true,
            records: u32::from(completed && was_valid),
            fenced: !completed,
            ..BankingWorkerStepReport::default()
        })
    }

    pub async fn run_statement_once(
        &self,
        holder: &str,
        now: DateTime<Utc>,
    ) -> Result<BankingWorkerStepReport, BankingError> {
        let Some(work) = self
            .workers
            .claim_statement_window(holder, now, LEASE_SECONDS)
            .await?
        else {
            return Ok(BankingWorkerStepReport::default());
        };
        let item_span = tracing::info_span!(
            "worker.item",
            operation = "banking.statement",
            connection_id = %work.connection_id,
            sync_job_id = %work.sync_job_id,
            resource_id = %work.resource_id,
        );
        log_claimed(&item_span);
        let credential = match self.cipher.decrypt(
            &work.provider_envelope,
            &CredentialBinding::new(
                work.user_id,
                work.connection_id.into_uuid(),
                &work.provider,
                work.credential_generation,
                "active",
            )?,
        ) {
            Ok(credential) => credential,
            Err(_) => {
                return self
                    .finish_statement_failure(
                        &work,
                        ProviderFailure::Classified {
                            class: ProviderFailureClass::Terminal,
                        },
                        now,
                    )
                    .await;
            }
        };
        let body = match self
            .provider
            .statement(&credential, &work.external_resource_id, work.from, work.to)
            .instrument(item_span)
            .await
        {
            Ok(body) => body,
            Err(failure) => return self.finish_statement_failure(&work, failure, now).await,
        };
        let currencies = self.currency_map().await?;
        let events = match self
            .normalizer
            .statement(&body, &work.resource_currency, &currencies)
        {
            Ok(events) => events,
            Err(_) => {
                return self
                    .finish_statement_failure(&work, ProviderFailure::InvalidResponse, now)
                    .await;
            }
        };
        let completed_at = completion_time(now);
        let records = self
            .workers
            .complete_statement_window(&work, &events, completed_at)
            .await?;
        Ok(BankingWorkerStepReport {
            claimed: true,
            records,
            ..BankingWorkerStepReport::default()
        })
    }

    pub async fn finalize_sync_page_once(
        &self,
        now: DateTime<Utc>,
    ) -> Result<BankingWorkerStepReport, BankingError> {
        let finalized = self.workers.finalize_one_sync_page(now).await?;
        Ok(BankingWorkerStepReport {
            claimed: finalized,
            records: u32::from(finalized),
            ..BankingWorkerStepReport::default()
        })
    }

    async fn currency_map(&self) -> Result<ProviderCurrencyMap, BankingError> {
        use crate::contexts::reference_data::public::CurrencyCatalog;
        Ok(self
            .currencies
            .list_known()
            .await
            .map_err(|_| BankingError::InvalidValue("currency catalog unavailable"))?
            .into_iter()
            .filter_map(|definition| {
                definition
                    .numeric_code
                    .and_then(|numeric| numeric.parse::<u16>().ok())
                    .map(|numeric| {
                        (
                            numeric,
                            ProviderCurrency {
                                code: definition.code,
                                minor_unit: definition.minor_unit,
                                enabled: definition.enabled,
                            },
                        )
                    })
            })
            .collect())
    }

    fn provision_webhook(
        &self,
        work: &super::ValidationWork,
        version: i64,
    ) -> Result<WebhookProvisioning, BankingError> {
        let credential = self.webhook_secrets.generate();
        let digest = self.webhook_secrets.digest(&credential);
        let secret = ProviderCredential::new(credential.expose())?;
        let envelope = self.cipher.encrypt(
            &secret,
            &CredentialBinding::new(
                work.user_id,
                work.connection_id.into_uuid(),
                &work.provider,
                version,
                "webhook",
            )?,
        )?;
        Ok(WebhookProvisioning {
            version,
            envelope,
            digest,
        })
    }

    async fn finish_validation_failure(
        &self,
        work: &super::ValidationWork,
        failure: ProviderFailure,
        now: DateTime<Utc>,
    ) -> Result<BankingWorkerStepReport, BankingError> {
        let completed_at = completion_time(now);
        let next_retry_at = retry_at(work.attempts, &failure, completed_at);
        let completed = self
            .workers
            .complete_validation_failure(work, failure.class(), next_retry_at, completed_at)
            .await?;
        Ok(BankingWorkerStepReport {
            claimed: true,
            retry_scheduled: completed && next_retry_at.is_some(),
            fenced: !completed,
            ..BankingWorkerStepReport::default()
        })
    }

    async fn finish_statement_failure(
        &self,
        work: &super::StatementWork,
        failure: ProviderFailure,
        now: DateTime<Utc>,
    ) -> Result<BankingWorkerStepReport, BankingError> {
        let completed_at = completion_time(now);
        let next_retry_at = retry_at(work.attempts, &failure, completed_at);
        let completed = self
            .workers
            .fail_statement_window(work, failure.class(), next_retry_at, completed_at)
            .await?;
        Ok(BankingWorkerStepReport {
            claimed: true,
            retry_scheduled: completed && next_retry_at.is_some(),
            fenced: !completed,
            ..BankingWorkerStepReport::default()
        })
    }
}

fn log_claimed(span: &tracing::Span) {
    span.in_scope(|| {
        tracing::info!(
            event.name = "worker.item.claimed",
            outcome = "claimed",
            "Worker item claimed"
        );
    });
}

fn completion_time(claimed_at: DateTime<Utc>) -> DateTime<Utc> {
    Utc::now().max(claimed_at)
}

fn retry_at(attempts: i32, failure: &ProviderFailure, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    if attempts >= MAX_ATTEMPTS
        || !matches!(
            failure.class(),
            ProviderFailureClass::RateLimited | ProviderFailureClass::Transient
        )
    {
        return None;
    }
    let exponent = u32::try_from((attempts - 1).max(0)).unwrap_or(0).min(16);
    let exponential = MIN_RETRY_SECONDS
        .saturating_mul(1_i64.checked_shl(exponent).unwrap_or(i64::MAX))
        .min(MAX_RETRY_SECONDS);
    let retry_after = failure
        .retry_after_seconds()
        .and_then(|seconds| i64::try_from(seconds).ok())
        .unwrap_or(0)
        .min(MAX_RETRY_SECONDS);
    Some(now + Duration::seconds(exponential.max(retry_after)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_retry_is_exponential_bounded_and_stops_after_ten_attempts() {
        let now = DateTime::from_timestamp(1_800_000_000, 0).unwrap();
        let transient = ProviderFailure::Classified {
            class: ProviderFailureClass::Transient,
        };
        assert_eq!(
            retry_at(1, &transient, now),
            Some(now + Duration::seconds(61))
        );
        assert_eq!(
            retry_at(7, &transient, now),
            Some(now + Duration::seconds(3_600))
        );
        assert_eq!(retry_at(10, &transient, now), None);
        assert_eq!(
            retry_at(
                1,
                &ProviderFailure::ClassifiedWithRetry {
                    class: ProviderFailureClass::RateLimited,
                    retry_after_seconds: 900,
                },
                now,
            ),
            Some(now + Duration::seconds(900))
        );
        assert_eq!(
            retry_at(
                1,
                &ProviderFailure::ClassifiedWithRetry {
                    class: ProviderFailureClass::RateLimited,
                    retry_after_seconds: 86_400,
                },
                now,
            ),
            Some(now + Duration::seconds(3_600))
        );
    }

    #[test]
    fn credential_and_terminal_failures_are_not_retried() {
        let now = DateTime::from_timestamp(1_800_000_000, 0).unwrap();
        for class in [
            ProviderFailureClass::NeedsReauth,
            ProviderFailureClass::Terminal,
        ] {
            assert_eq!(
                retry_at(1, &ProviderFailure::Classified { class }, now),
                None
            );
        }
    }
}
