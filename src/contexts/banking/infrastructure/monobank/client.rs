use async_trait::async_trait;

use crate::contexts::banking::application::{
    ProviderClient, ProviderCredential, ProviderFailure, ProviderFailureClass,
};

#[derive(Clone)]
pub struct MonobankClient {
    client: reqwest::Client,
    base_url: String,
}

impl MonobankClient {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(25))
                .build()
                .expect("static Monobank HTTP client configuration is valid"),
            base_url: base_url.into(),
        }
    }
}

#[async_trait]
impl ProviderClient for MonobankClient {
    async fn client_info(
        &self,
        credential: &ProviderCredential,
    ) -> Result<String, ProviderFailure> {
        let response = self
            .client
            .get(format!(
                "{}/personal/client-info",
                self.base_url.trim_end_matches('/')
            ))
            .header("X-Token", credential.expose())
            .send()
            .await
            .map_err(|_| ProviderFailure::Classified {
                class: crate::contexts::banking::application::ProviderFailureClass::Transient,
            })?;
        if !response.status().is_success() {
            return Err(failure(&response));
        }
        response
            .text()
            .await
            .map_err(|_| ProviderFailure::InvalidResponse)
    }

    async fn register_webhook(
        &self,
        credential: &ProviderCredential,
        callback_url: &str,
    ) -> Result<(), ProviderFailure> {
        let response = self
            .client
            .post(format!(
                "{}/personal/webhook",
                self.base_url.trim_end_matches('/')
            ))
            .header("X-Token", credential.expose())
            .json(&serde_json::json!({"webHookUrl":callback_url}))
            .send()
            .await
            .map_err(|_| ProviderFailure::Classified {
                class: crate::contexts::banking::application::ProviderFailureClass::Transient,
            })?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(failure(&response))
        }
    }

    async fn statement(
        &self,
        credential: &ProviderCredential,
        account: &str,
        from: chrono::DateTime<chrono::Utc>,
        to: chrono::DateTime<chrono::Utc>,
    ) -> Result<String, ProviderFailure> {
        let mut url =
            reqwest::Url::parse(&self.base_url).map_err(|_| ProviderFailure::Classified {
                class: ProviderFailureClass::Terminal,
            })?;
        {
            let mut segments =
                url.path_segments_mut()
                    .map_err(|_| ProviderFailure::Classified {
                        class: ProviderFailureClass::Terminal,
                    })?;
            segments.pop_if_empty();
            segments.extend([
                "personal",
                "statement",
                account,
                &from.timestamp().to_string(),
                &to.timestamp().to_string(),
            ]);
        }
        let response = self
            .client
            .get(url)
            .header("X-Token", credential.expose())
            .send()
            .await
            .map_err(|_| ProviderFailure::Classified {
                class: ProviderFailureClass::Transient,
            })?;
        if !response.status().is_success() {
            return Err(failure(&response));
        }
        response
            .text()
            .await
            .map_err(|_| ProviderFailure::InvalidResponse)
    }
}

fn failure(response: &reqwest::Response) -> ProviderFailure {
    let class = super::MonobankAdapter::classify_status(response.status().as_u16());
    let retry_after_seconds = response
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| {
            value.parse::<u64>().ok().or_else(|| {
                chrono::DateTime::parse_from_rfc2822(value)
                    .ok()
                    .and_then(|at| {
                        u64::try_from(
                            (at.with_timezone(&chrono::Utc) - chrono::Utc::now())
                                .num_seconds()
                                .max(0),
                        )
                        .ok()
                    })
            })
        });
    match retry_after_seconds {
        Some(retry_after_seconds) => ProviderFailure::ClassifiedWithRetry {
            class,
            retry_after_seconds,
        },
        None => ProviderFailure::Classified { class },
    }
}
