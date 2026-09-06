//! Shared HTTP identity and request-correlation boundaries.

use std::time::{Duration, Instant};

use axum::extract::{MatchedPath, Request};
use axum::http::{HeaderValue, StatusCode, header::HeaderName};
use axum::middleware::Next;
use axum::response::Response;
use tracing::Instrument as _;
use uuid::Uuid;

use crate::shared_kernel::CorrelationId;

const X_REQUEST_ID: HeaderName = HeaderName::from_static("x-request-id");

/// Authenticated tenant identity.
#[derive(Clone, Debug)]
pub struct AuthUser(pub Uuid);

/// Generates a command correlation id and records it on the active request span.
pub fn request_correlation_id() -> CorrelationId {
    let correlation_id = CorrelationId::generate();
    tracing::Span::current().record("correlation_id", tracing::field::display(correlation_id));
    correlation_id
}

/// Correlates and safely logs one HTTP request without recording its raw URI.
pub async fn trace_request(mut request: Request, next: Next) -> Response {
    let request_id = select_request_id(request.headers().get(&X_REQUEST_ID));
    request
        .headers_mut()
        .insert(X_REQUEST_ID, request_id.clone());
    let method = request.method().clone();
    let route = route_template(&request).to_owned();
    let request_id_text = request_id
        .to_str()
        .expect("generated or validated request id is visible ASCII")
        .to_owned();
    let span = tracing::info_span!(
        "http.request",
        request_id = %request_id_text,
        correlation_id = tracing::field::Empty,
        http.method = %method,
        http.route = %route,
    );

    async move {
        let started = Instant::now();
        let mut response = next.run(request).await;
        let status = response.status();
        response.headers_mut().insert(X_REQUEST_ID, request_id);
        log_completion(&route, status, started.elapsed());
        response
    }
    .instrument(span)
    .await
}

fn route_template(request: &Request) -> &str {
    request
        .extensions()
        .get::<MatchedPath>()
        .map(MatchedPath::as_str)
        .unwrap_or("<unmatched>")
}

fn select_request_id(candidate: Option<&HeaderValue>) -> HeaderValue {
    candidate
        .filter(|value| valid_request_id(value))
        .cloned()
        .unwrap_or_else(|| {
            HeaderValue::from_str(&Uuid::new_v4().to_string())
                .expect("UUID request ids are valid header values")
        })
}

fn valid_request_id(value: &HeaderValue) -> bool {
    value.to_str().is_ok_and(|value| {
        !value.is_empty()
            && value.len() <= 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    })
}

fn log_completion(route: &str, status: StatusCode, elapsed: Duration) {
    let status = status.as_u16();
    let duration_ms = u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX);
    let expected_health =
        matches!(route, "/health/live" | "/health/ready") && matches!(status, 200 | 503);
    if expected_health {
        tracing::debug!(
            event.name = "http.request.completed",
            http.status = status,
            duration_ms,
            "HTTP request completed"
        );
    } else if status >= 500 {
        tracing::error!(
            event.name = "http.request.completed",
            http.status = status,
            duration_ms,
            "HTTP request completed"
        );
    } else {
        tracing::info!(
            event.name = "http.request.completed",
            http.status = status,
            duration_ms,
            "HTTP request completed"
        );
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::sync::{Arc, Mutex};

    use axum::Router;
    use axum::body::Bytes;
    use axum::http::{HeaderValue, Request, StatusCode};
    use axum::middleware;
    use axum::routing::post;
    use axum_test::TestServer;
    use tracing_subscriber::fmt::MakeWriter;

    use super::{route_template, select_request_id, trace_request, valid_request_id};

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

    #[test]
    fn request_ids_accept_only_bounded_log_safe_characters() {
        for valid in ["client-123", "trace_id.7", "A"] {
            assert!(valid_request_id(&HeaderValue::from_str(valid).unwrap()));
            assert_eq!(
                select_request_id(Some(&HeaderValue::from_str(valid).unwrap())),
                valid
            );
        }
        for invalid in ["", "contains space", "slash/value", &"a".repeat(65)] {
            assert!(!valid_request_id(&HeaderValue::from_str(invalid).unwrap()));
            let selected = select_request_id(Some(&HeaderValue::from_str(invalid).unwrap()));
            assert_ne!(selected, invalid);
            assert!(Uuid::parse_str(selected.to_str().unwrap()).is_ok());
        }
    }

    #[test]
    fn unmatched_raw_targets_are_never_used_as_log_routes() {
        let request = Request::builder()
            .uri("/webhooks/monobank/sentinel-secret?code=oauth-sentinel")
            .header("authorization", "Bearer header-sentinel")
            .body(axum::body::Body::from("body-sentinel"))
            .unwrap();
        assert_eq!(route_template(&request), "<unmatched>");
        assert!(!route_template(&request).contains("sentinel"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn completed_request_logs_only_the_matched_template_and_safe_fields() {
        let output = Buffer::default();
        let subscriber = tracing_subscriber::fmt()
            .json()
            .flatten_event(true)
            .with_current_span(true)
            .with_ansi(false)
            .with_writer(output.clone())
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);
        let router = Router::new()
            .route(
                "/webhooks/monobank/{credential}",
                post(|| async { StatusCode::NO_CONTENT }),
            )
            .layer(middleware::from_fn(trace_request));
        let server = TestServer::new(router).unwrap();

        let response = server
            .post("/webhooks/monobank/path-sentinel?code=query-sentinel")
            .add_header("x-request-id", "request-safe-123")
            .add_header("authorization", "Bearer header-sentinel")
            .bytes(Bytes::from_static(b"body-sentinel"))
            .await;
        assert_eq!(response.status_code(), StatusCode::NO_CONTENT);

        let logs = String::from_utf8(output.0.lock().unwrap().clone()).unwrap();
        assert!(logs.contains("http.request.completed"));
        assert!(logs.contains("/webhooks/monobank/{credential}"));
        assert!(logs.contains("request-safe-123"));
        for sentinel in [
            "path-sentinel",
            "query-sentinel",
            "header-sentinel",
            "body-sentinel",
        ] {
            assert!(!logs.contains(sentinel), "logs exposed {sentinel}");
        }
    }

    use uuid::Uuid;
}
