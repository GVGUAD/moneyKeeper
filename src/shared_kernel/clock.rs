//! Injectable UTC wall clocks.

use chrono::{DateTime, Utc};

/// Supplies the current UTC wall-clock time to domain and application code.
pub trait Clock: Send + Sync {
    /// Returns the current UTC instant.
    fn now(&self) -> DateTime<Utc>;
}

/// The production UTC wall clock.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

/// A deterministic clock that always returns one injected UTC instant.
#[derive(Clone, Debug)]
pub struct FixedClock {
    instant: DateTime<Utc>,
}

impl FixedClock {
    /// Creates a clock fixed at `instant`.
    pub const fn new(instant: DateTime<Utc>) -> Self {
        Self { instant }
    }
}

impl Clock for FixedClock {
    fn now(&self) -> DateTime<Utc> {
        self.instant
    }
}
