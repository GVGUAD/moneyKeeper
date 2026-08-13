//! Business-neutral primitives shared by Finance V2 bounded contexts.
//!
//! This module intentionally contains no repositories, provider concepts, or
//! context-specific aggregate identifiers.

mod clock;
mod currency;
mod events;
mod idempotency;
mod ids;
mod money;

pub use crate::define_uuid_id;
pub use clock::{Clock, FixedClock, SystemClock};
pub use currency::{CurrencyCode, CurrencyCodeError};
pub use events::{
    EventEnvelope, EventEnvelopeError, MAX_EVENT_AGGREGATE_ID_BYTES, MAX_EVENT_CONTEXT_BYTES,
    MAX_EVENT_TYPE_BYTES,
};
pub use idempotency::{IdempotencyKey, IdempotencyKeyError};
pub use ids::{CausationId, CorrelationId, EventId, UserId};
pub use money::{Money, MoneyError};
