use async_trait::async_trait;
use std::time::{Duration, Instant};

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
        let started = Instant::now();
        let response = self
            .client
            .get(format!(
                "{}/personal/client-info",
                self.base_url.trim_end_matches('/')
            ))
            .header("X-Token", credential.expose())
            .send()
            .await
            .map_err(|error| {
                log_transport_failure("client_info", &error, started.elapsed());
                ProviderFailure::Classified {
                    class: ProviderFailureClass::Transient,
                }
            })?;
        let status = response.status();
        if !status.is_success() {
            log_http_failure("client_info", status, started.elapsed());
            return Err(failure(&response));
        }
        let body = response.text().await.map_err(|_| {
            log_invalid_response("client_info", status, started.elapsed());
            ProviderFailure::InvalidResponse
        })?;
        log_success("client_info", status, started.elapsed());
        Ok(body)
    }

    async fn register_webhook(
        &self,
        credential: &ProviderCredential,
        callback_url: &str,
    ) -> Result<(), ProviderFailure> {
        let started = Instant::now();
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
            .map_err(|error| {
                log_transport_failure("register_webhook", &error, started.elapsed());
                ProviderFailure::Classified {
                    class: ProviderFailureClass::Transient,
                }
            })?;
        let status = response.status();
        if status.is_success() {
            log_success("register_webhook", status, started.elapsed());
            Ok(())
        } else {
            log_http_failure("register_webhook", status, started.elapsed());
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
        let started = Instant::now();
        let response = self
            .client
            .get(url)
            .header("X-Token", credential.expose())
            .send()
            .await
            .map_err(|error| {
                log_transport_failure("statement", &error, started.elapsed());
                ProviderFailure::Classified {
                    class: ProviderFailureClass::Transient,
                }
            })?;
        let status = response.status();
        if !status.is_success() {
            log_http_failure("statement", status, started.elapsed());
            return Err(failure(&response));
        }
        let body = response.text().await.map_err(|_| {
            log_invalid_response("statement", status, started.elapsed());
            ProviderFailure::InvalidResponse
        })?;
        log_success("statement", status, started.elapsed());
        Ok(body)
    }
}

fn log_success(operation: &'static str, status: reqwest::StatusCode, elapsed: Duration) {
    tracing::info!(
        event.name = "provider.request.completed",
        provider = "monobank",
        operation,
        outcome = "success",
        http.status = status.as_u16(),
        duration_ms = elapsed_ms(elapsed),
        "Provider request completed"
    );
}

fn log_http_failure(operation: &'static str, status: reqwest::StatusCode, elapsed: Duration) {
    tracing::warn!(
        event.name = "provider.request.completed",
        provider = "monobank",
        operation,
        outcome = "http_error",
        http.status = status.as_u16(),
        failure.class = ?super::MonobankAdapter::classify_status(status.as_u16()),
        duration_ms = elapsed_ms(elapsed),
        "Provider request completed"
    );
}

fn log_transport_failure(operation: &'static str, error: &reqwest::Error, elapsed: Duration) {
    tracing::warn!(
        event.name = "provider.request.completed",
        provider = "monobank",
        operation,
        outcome = transport_outcome(error),
        duration_ms = elapsed_ms(elapsed),
        "Provider request completed"
    );
}

fn log_invalid_response(operation: &'static str, status: reqwest::StatusCode, elapsed: Duration) {
    tracing::warn!(
        event.name = "provider.request.completed",
        provider = "monobank",
        operation,
        outcome = "invalid_response",
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

pub fn elapsed_ms(elapsed: Duration) -> u64 {
    u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
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

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::sync::{Arc, Mutex};

    use axum::Router;
    use axum::http::{HeaderValue, StatusCode};
    use axum::response::IntoResponse;
    use axum::routing::{get, post};
    use chrono::{TimeZone as _, Utc};
    use tracing::instrument::WithSubscriber as _;
    use tracing_subscriber::fmt::MakeWriter;

    use super::*;

    #[derive(Clone, Default)]
    struct Buffer(Arc<Mutex<Vec<u8>>>);

    struct BufferWriter(Arc<Mutex<Vec<u8>>>);

    impl Write for BufferWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'writer> MakeWriter<'writer> for Buffer {
        type Writer = BufferWriter;

        fn make_writer(&'writer self) -> Self::Writer {
            BufferWriter(Arc::clone(&self.0))
        }
    }

    impl Buffer {
        fn text(&self) -> String {
            String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
        }
    }

    #[tokio::test]
    async fn provider_logs_keep_tokens_urls_external_ids_and_bodies_redacted() {
        let app = Router::new()
            .route(
                "/personal/client-info",
                get(|| async { "provider-body-sentinel-success" }),
            )
            .route("/personal/webhook", post(|| async { StatusCode::OK }))
            .route(
                "/personal/statement/{account}/{from}/{to}",
                get(|| async {
                    let mut response = (
                        StatusCode::TOO_MANY_REQUESTS,
                        "provider-body-sentinel-failure",
                    )
                        .into_response();
                    response
                        .headers_mut()
                        .insert("retry-after", HeaderValue::from_static("120"));
                    response
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let output = Buffer::default();
        let subscriber = tracing_subscriber::fmt()
            .json()
            .flatten_event(true)
            .with_ansi(false)
            .with_writer(output.clone())
            .finish();
        let client = MonobankClient::new(format!("http://{address}"));
        let credential = ProviderCredential::new("token-sentinel-secret").unwrap();
        async {
            let body = client.client_info(&credential).await.unwrap();
            assert_eq!(body, "provider-body-sentinel-success");
            client
                .register_webhook(
                    &credential,
                    "https://example.test/webhooks/callback-sentinel-secret",
                )
                .await
                .unwrap();
            let failure = client
                .statement(
                    &credential,
                    "external-account-sentinel",
                    Utc.timestamp_opt(1_800_000_000, 0).unwrap(),
                    Utc.timestamp_opt(1_800_000_100, 0).unwrap(),
                )
                .await
                .unwrap_err();
            assert_eq!(failure.class(), ProviderFailureClass::RateLimited);
            assert_eq!(failure.retry_after_seconds(), Some(120));
        }
        .with_subscriber(subscriber)
        .await;
        server.abort();

        let logs = output.text();
        assert!(logs.contains("provider.request.completed"));
        assert!(logs.contains("client_info"));
        assert!(logs.contains("register_webhook"));
        assert!(logs.contains("statement"));
        for sentinel in [
            "token-sentinel-secret",
            "callback-sentinel-secret",
            "external-account-sentinel",
            "provider-body-sentinel-success",
            "provider-body-sentinel-failure",
        ] {
            assert!(!logs.contains(sentinel), "leaked {sentinel}");
        }
    }
}
