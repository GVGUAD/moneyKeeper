//! Provider-neutral classification capability.

use std::fmt;
use std::time::Duration;

use async_trait::async_trait;

use super::model::{ClassificationEvidence, Prediction};

/// Stable provider failure classes used by retry policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClassifierFailureClass {
    Transient,
    RateLimited,
    Terminal,
    InvalidResponse,
    Configuration,
}

/// A deliberately redacted provider failure.
#[derive(Clone)]
pub struct ClassifierError {
    class: ClassifierFailureClass,
    retry_after: Option<Duration>,
    message: &'static str,
}

impl ClassifierError {
    pub const fn transient(message: &'static str) -> Self {
        Self {
            class: ClassifierFailureClass::Transient,
            retry_after: None,
            message,
        }
    }

    pub const fn rate_limited(retry_after: Option<Duration>) -> Self {
        Self {
            class: ClassifierFailureClass::RateLimited,
            retry_after,
            message: "classification provider rate limit reached",
        }
    }

    pub const fn terminal(message: &'static str) -> Self {
        Self {
            class: ClassifierFailureClass::Terminal,
            retry_after: None,
            message,
        }
    }

    pub const fn invalid_response() -> Self {
        Self {
            class: ClassifierFailureClass::InvalidResponse,
            retry_after: None,
            message: "classification provider returned an invalid response",
        }
    }

    pub const fn configuration(message: &'static str) -> Self {
        Self {
            class: ClassifierFailureClass::Configuration,
            retry_after: None,
            message,
        }
    }

    pub const fn class(&self) -> ClassifierFailureClass {
        self.class
    }

    pub const fn retry_after(&self) -> Option<Duration> {
        self.retry_after
    }

    pub const fn is_retryable(&self) -> bool {
        matches!(
            self.class,
            ClassifierFailureClass::Transient
                | ClassifierFailureClass::RateLimited
                | ClassifierFailureClass::InvalidResponse
        )
    }
}

impl fmt::Debug for ClassifierError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ClassifierError")
            .field("class", &self.class)
            .field("retry_after", &self.retry_after)
            .field("details", &"[REDACTED]")
            .finish()
    }
}

impl fmt::Display for ClassifierError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for ClassifierError {}

/// A replaceable model-provider port. Each invocation classifies exactly one transaction.
#[async_trait]
pub trait TransactionClassifier: Send + Sync {
    fn provider_name(&self) -> &str;

    fn model_name(&self) -> &str;

    async fn classify(
        &self,
        evidence: &ClassificationEvidence,
    ) -> Result<Prediction, ClassifierError>;
}
