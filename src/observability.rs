//! Process-wide tracing configuration.

use anyhow::Context as _;
use tracing_subscriber::EnvFilter;

const DEFAULT_FILTER: &str = "moneykeeper=info";

/// Supported log encodings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogFormat {
    /// Compact, human-readable events for local terminals.
    Compact,
    /// Flattened JSON events for log aggregation.
    Json,
}

impl LogFormat {
    fn parse(value: Option<&str>) -> anyhow::Result<Self> {
        match value {
            None | Some("compact") => Ok(Self::Compact),
            Some("json") => Ok(Self::Json),
            Some(value) => {
                anyhow::bail!("LOG_FORMAT must be either compact or json, received {value:?}")
            }
        }
    }

    /// Loads the configured format, defaulting to compact output.
    pub fn from_environment() -> anyhow::Result<Self> {
        Self::parse(std::env::var("LOG_FORMAT").ok().as_deref())
    }
}

/// Installs the global tracing subscriber.
pub fn initialize() -> anyhow::Result<LogFormat> {
    let format = LogFormat::from_environment()?;
    let filter = match std::env::var("RUST_LOG") {
        Ok(value) => EnvFilter::try_new(value).context("RUST_LOG contains an invalid filter")?,
        Err(std::env::VarError::NotPresent) => EnvFilter::new(DEFAULT_FILTER),
        Err(error) => return Err(error).context("read RUST_LOG"),
    };

    match format {
        LogFormat::Compact => tracing_subscriber::fmt()
            .compact()
            .with_env_filter(filter)
            .try_init()
            .map_err(|error| anyhow::anyhow!("install compact tracing subscriber: {error}"))?,
        LogFormat::Json => tracing_subscriber::fmt()
            .json()
            .flatten_event(true)
            .with_current_span(true)
            .with_span_list(true)
            .with_env_filter(filter)
            .try_init()
            .map_err(|error| anyhow::anyhow!("install JSON tracing subscriber: {error}"))?,
    }
    Ok(format)
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::sync::{Arc, Mutex};

    use tracing_subscriber::fmt::MakeWriter;

    use super::{DEFAULT_FILTER, LogFormat};

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

    #[test]
    fn log_format_defaults_to_compact_and_accepts_json() {
        assert_eq!(LogFormat::parse(None).unwrap(), LogFormat::Compact);
        assert_eq!(
            LogFormat::parse(Some("compact")).unwrap(),
            LogFormat::Compact
        );
        assert_eq!(LogFormat::parse(Some("json")).unwrap(), LogFormat::Json);
    }

    #[test]
    fn log_format_rejects_unknown_or_empty_values() {
        for value in ["", "pretty", "JSON"] {
            assert!(LogFormat::parse(Some(value)).is_err(), "{value:?}");
        }
    }

    #[test]
    fn default_filter_keeps_dependency_noise_disabled() {
        assert_eq!(DEFAULT_FILTER, "moneykeeper=info");
    }

    #[test]
    fn json_output_flattens_searchable_event_fields() {
        let output = Buffer::default();
        let subscriber = tracing_subscriber::fmt()
            .json()
            .flatten_event(true)
            .with_ansi(false)
            .with_writer(output.clone())
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(
                event.name = "test.event",
                request_id = "request-123",
                duration_ms = 7_u64,
                "test event"
            );
        });
        let event: serde_json::Value = serde_json::from_str(output.text().trim()).unwrap();
        assert_eq!(event["event.name"], "test.event");
        assert_eq!(event["request_id"], "request-123");
        assert_eq!(event["duration_ms"], 7);
    }

    #[test]
    fn compact_output_remains_human_readable() {
        let output = Buffer::default();
        let subscriber = tracing_subscriber::fmt()
            .compact()
            .with_ansi(false)
            .with_writer(output.clone())
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(event.name = "test.event", "compact sentinel");
        });
        let output = output.text();
        assert!(output.contains("compact sentinel"));
        assert!(output.contains("event.name=\"test.event\""));
    }
}
