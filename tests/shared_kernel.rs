use chrono::{TimeZone, Utc};
use moneykeeper::shared_kernel::{
    CausationId, Clock, CorrelationId, CurrencyCode, CurrencyCodeError, EventEnvelope,
    EventEnvelopeError, EventId, FixedClock, IdempotencyKey, IdempotencyKeyError,
    MAX_EVENT_AGGREGATE_ID_BYTES, MAX_EVENT_CONTEXT_BYTES, MAX_EVENT_TYPE_BYTES, Money, MoneyError,
    UserId, define_uuid_id,
};
use rust_decimal::Decimal;
use serde::Deserialize;
use sqlx::{Decode, Encode, Postgres, Type};
use std::{collections::HashSet, str::FromStr};
use uuid::Uuid;

define_uuid_id!(pub TestOnlyId);

fn assert_copy<T: Copy>() {}

fn assert_postgres_uuid_traits<T>()
where
    T: Type<Postgres>,
    for<'query> T: Encode<'query, Postgres>,
    for<'row> T: Decode<'row, Postgres>,
{
}

#[test]
fn universal_identifiers_are_distinct_uuid_value_types() {
    assert_copy::<UserId>();
    assert_copy::<EventId>();
    assert_copy::<CorrelationId>();
    assert_copy::<CausationId>();
    assert_postgres_uuid_traits::<UserId>();
    assert_postgres_uuid_traits::<EventId>();
    assert_postgres_uuid_traits::<CorrelationId>();
    assert_postgres_uuid_traits::<CausationId>();

    let raw = Uuid::parse_str("018f8f6f-b6fd-7c53-a6c4-7b7fd9d6e2aa").unwrap();
    let user_id = UserId::new(raw);
    let event_id = EventId::from_uuid(raw);

    assert_eq!(user_id.as_uuid(), &raw);
    assert_eq!(event_id.into_uuid(), raw);
    assert_eq!(user_id.to_string(), raw.to_string());
    assert_eq!(
        serde_json::to_string(&user_id).unwrap(),
        format!("\"{raw}\"")
    );
    assert_eq!(
        serde_json::from_str::<UserId>(&format!("\"{raw}\"")).unwrap(),
        user_id
    );
    assert_eq!(UserId::from_str(&raw.to_string()).unwrap(), user_id);

    let mut values = HashSet::new();
    values.insert(user_id);
    assert!(values.contains(&UserId::new(raw)));

    assert_eq!(UserId::generate().into_uuid().get_version_num(), 4);
}

#[test]
fn reusable_id_macro_creates_an_opaque_real_type() {
    assert_copy::<TestOnlyId>();
    assert_postgres_uuid_traits::<TestOnlyId>();

    let raw = Uuid::new_v4();
    let id = TestOnlyId::new(raw);
    assert_eq!(id.as_uuid(), &raw);
    assert_eq!(id.to_string(), raw.to_string());
    assert_eq!(
        serde_json::from_str::<TestOnlyId>(&serde_json::to_string(&id).unwrap()).unwrap(),
        id
    );
}

#[test]
fn idempotency_key_preserves_valid_opaque_bytes_and_redacts_debug() {
    let first = IdempotencyKey::new("checkout attempt:01").unwrap();
    let second = IdempotencyKey::new("checkout attempt:01").unwrap();

    assert_eq!(first, second);
    assert_eq!(first.as_str(), "checkout attempt:01");
    assert_eq!(first.len(), 19);
    assert_eq!(format!("{first:?}"), "IdempotencyKey([REDACTED])");
    assert!(!format!("{first:?}").contains(first.as_str()));
    assert_eq!(
        serde_json::from_str::<IdempotencyKey>(&serde_json::to_string(&first).unwrap()).unwrap(),
        first
    );
}

#[test]
fn idempotency_key_rejects_invalid_text_by_utf8_byte_length() {
    assert_eq!(IdempotencyKey::new(""), Err(IdempotencyKeyError::Empty));
    assert_eq!(
        IdempotencyKey::new(" leading"),
        Err(IdempotencyKeyError::SurroundingWhitespace)
    );
    assert_eq!(
        IdempotencyKey::new("trailing "),
        Err(IdempotencyKeyError::SurroundingWhitespace)
    );
    assert_eq!(
        IdempotencyKey::new("line\nbreak"),
        Err(IdempotencyKeyError::ControlCharacter)
    );
    assert_eq!(
        IdempotencyKey::new("é".repeat(101)),
        Err(IdempotencyKeyError::TooLong {
            actual_bytes: 202,
            max_bytes: 200,
        })
    );
    assert!(IdempotencyKey::new("é".repeat(100)).is_ok());
    assert!(serde_json::from_str::<IdempotencyKey>(r#"" leading""#).is_err());
    assert!(serde_json::from_value::<IdempotencyKey>(serde_json::json!("é".repeat(101))).is_err());
}

#[test]
fn currency_code_accepts_only_canonical_uppercase_ascii() {
    for accepted in ["UAH", "USD", "EUR"] {
        let code = CurrencyCode::new(accepted).unwrap();
        assert_eq!(code.as_str(), accepted);
        assert_eq!(code.to_string(), accepted);
        assert_eq!(
            serde_json::to_string(&code).unwrap(),
            format!("\"{accepted}\"")
        );
    }

    for rejected in ["uah", " UAH", "UAH ", "US", "USDD", "U1D", "ЄВР"] {
        assert_eq!(
            CurrencyCode::new(rejected),
            Err(CurrencyCodeError::InvalidFormat),
            "{rejected:?} should be rejected"
        );
    }
    assert!(serde_json::from_str::<CurrencyCode>("\"usd\"").is_err());
}

#[test]
fn validated_string_value_objects_have_no_transparent_sqlx_decode_path() {
    let currency_source = include_str!("../src/shared_kernel/currency.rs");
    let idempotency_source = include_str!("../src/shared_kernel/idempotency.rs");

    for (name, source) in [
        ("CurrencyCode", currency_source),
        ("IdempotencyKey", idempotency_source),
    ] {
        assert!(
            !source.contains("sqlx(transparent)"),
            "{name} must not transparently decode unchecked database text"
        );
        assert!(
            !source.contains("sqlx::Type"),
            "{name} has no persistence call site requiring SQLx traits"
        );
    }
}

fn money(amount: &str, currency: &str, scale: u32) -> Result<Money, MoneyError> {
    Money::new(
        Decimal::from_str_exact(amount).unwrap(),
        CurrencyCode::new(currency).unwrap(),
        scale,
    )
}

#[test]
fn money_preserves_exact_decimal_strings_through_a_checked_wire_round_trip() {
    #[derive(Deserialize)]
    struct RawMoney {
        amount: String,
        currency: String,
    }

    let original = money("1250.00", "UAH", 2).unwrap();
    let json = serde_json::to_string(&original).unwrap();
    assert_eq!(json, r#"{"amount":"1250.00","currency":"UAH"}"#);

    let raw: RawMoney = serde_json::from_str(&json).unwrap();
    let rebuilt = Money::new(
        Decimal::from_str_exact(&raw.amount).unwrap(),
        CurrencyCode::new(raw.currency).unwrap(),
        2,
    )
    .unwrap();
    assert_eq!(rebuilt, original);
}

#[test]
fn money_rejects_invalid_scale_and_numeric_bounds() {
    assert_eq!(
        money("1.001", "USD", 2),
        Err(MoneyError::ExcessScale {
            actual: 3,
            allowed: 2,
        })
    );
    assert_eq!(
        money("1", "USD", 9),
        Err(MoneyError::InvalidMinorUnitScale { scale: 9, max: 8 })
    );

    assert!(money("99999999999999999999.99999999", "UAH", 8).is_ok());
    assert_eq!(
        money("100000000000000000000.00000000", "UAH", 8),
        Err(MoneyError::OutOfBounds)
    );
    assert_eq!(
        money("-100000000000000000000.00000000", "UAH", 8),
        Err(MoneyError::OutOfBounds)
    );
}

#[test]
fn money_arithmetic_is_checked_and_currency_safe() {
    let five = money("5.25", "USD", 2).unwrap();
    let two = money("2.00", "USD", 2).unwrap();
    assert_eq!(
        five.checked_add(&two).unwrap().amount(),
        Decimal::new(725, 2)
    );
    assert_eq!(
        five.checked_sub(&two).unwrap().amount(),
        Decimal::new(325, 2)
    );
    assert_eq!(five.checked_neg().unwrap().amount(), Decimal::new(-525, 2));
    assert_eq!(
        five.checked_add(&money("1.00", "EUR", 2).unwrap()),
        Err(MoneyError::CurrencyMismatch {
            left: CurrencyCode::new("USD").unwrap(),
            right: CurrencyCode::new("EUR").unwrap(),
        })
    );

    let maximum = money("99999999999999999999.99999999", "USD", 8).unwrap();
    let smallest = money("0.00000001", "USD", 8).unwrap();
    assert_eq!(maximum.checked_add(&smallest), Err(MoneyError::OutOfBounds));

    let zero = Money::zero(CurrencyCode::new("EUR").unwrap(), 2).unwrap();
    assert!(zero.is_zero());
    assert_eq!(zero.checked_neg().unwrap(), zero);
}

#[test]
fn fixed_clock_is_deterministic_across_a_command() {
    let instant = Utc.with_ymd_and_hms(2026, 8, 5, 10, 30, 0).unwrap();
    let clock = FixedClock::new(instant);
    assert_eq!(clock.now(), instant);
    assert_eq!(clock.now(), instant);
}

#[test]
fn event_envelope_carries_typed_versioned_metadata_without_payload() {
    let occurred_at = Utc.with_ymd_and_hms(2026, 8, 5, 10, 30, 0).unwrap();
    let event_id = EventId::generate();
    let user_id = UserId::generate();
    let correlation_id = CorrelationId::generate();
    let causation_id = CausationId::generate();
    let envelope = EventEnvelope::new(
        event_id,
        "ledger",
        "journal-entry:123",
        7,
        "ledger.journal-posted",
        1,
        user_id,
        occurred_at,
        correlation_id,
        Some(causation_id),
    )
    .unwrap();

    assert_eq!(envelope.event_id(), event_id);
    assert_eq!(envelope.context(), "ledger");
    assert_eq!(envelope.aggregate_id(), "journal-entry:123");
    assert_eq!(envelope.aggregate_version(), 7);
    assert_eq!(envelope.event_type(), "ledger.journal-posted");
    assert_eq!(envelope.schema_version(), 1);
    assert_eq!(envelope.user_id(), user_id);
    assert_eq!(envelope.occurred_at(), occurred_at);
    assert_eq!(envelope.correlation_id(), correlation_id);
    assert_eq!(envelope.causation_id(), Some(causation_id));

    let json = serde_json::to_value(&envelope).unwrap();
    assert_eq!(json["event_id"], event_id.to_string());
    assert_eq!(json["schema_version"], 1);
    assert!(json.get("payload").is_none());
    assert_eq!(
        serde_json::from_value::<EventEnvelope>(json).unwrap(),
        envelope
    );
}

#[test]
fn event_envelope_rejects_ambiguous_identity_and_zero_versions() {
    let make = |context: &str, aggregate_version: u64, schema_version: u32| {
        EventEnvelope::new(
            EventId::generate(),
            context,
            "aggregate:1",
            aggregate_version,
            "example.created",
            schema_version,
            UserId::generate(),
            Utc::now(),
            CorrelationId::generate(),
            None,
        )
    };

    assert_eq!(
        make("", 1, 1),
        Err(EventEnvelopeError::EmptyIdentity { field: "context" })
    );
    assert_eq!(
        make(" ledger", 1, 1),
        Err(EventEnvelopeError::InvalidIdentity { field: "context" })
    );
    assert_eq!(
        make("ledger", 0, 1),
        Err(EventEnvelopeError::ZeroAggregateVersion)
    );
    assert_eq!(
        make("ledger", 1, 0),
        Err(EventEnvelopeError::ZeroSchemaVersion)
    );
}

#[test]
fn event_envelope_enforces_persisted_utf8_byte_bounds() {
    let make = |context: String, aggregate_id: String, event_type: String| {
        EventEnvelope::new(
            EventId::generate(),
            context,
            aggregate_id,
            1,
            event_type,
            1,
            UserId::generate(),
            Utc::now(),
            CorrelationId::generate(),
            None,
        )
    };

    assert!(
        make(
            "é".repeat(MAX_EVENT_CONTEXT_BYTES / 2),
            "a".repeat(MAX_EVENT_AGGREGATE_ID_BYTES),
            "e".repeat(MAX_EVENT_TYPE_BYTES),
        )
        .is_ok()
    );

    assert_eq!(
        make(
            "é".repeat(MAX_EVENT_CONTEXT_BYTES / 2 + 1),
            "aggregate:1".to_owned(),
            "example.created".to_owned(),
        ),
        Err(EventEnvelopeError::IdentityTooLong {
            field: "context",
            actual_bytes: MAX_EVENT_CONTEXT_BYTES + 2,
            max_bytes: MAX_EVENT_CONTEXT_BYTES,
        })
    );
    assert_eq!(
        make(
            "ledger".to_owned(),
            "a".repeat(MAX_EVENT_AGGREGATE_ID_BYTES + 1),
            "example.created".to_owned(),
        ),
        Err(EventEnvelopeError::IdentityTooLong {
            field: "aggregate_id",
            actual_bytes: MAX_EVENT_AGGREGATE_ID_BYTES + 1,
            max_bytes: MAX_EVENT_AGGREGATE_ID_BYTES,
        })
    );
    assert_eq!(
        make(
            "ledger".to_owned(),
            "aggregate:1".to_owned(),
            "e".repeat(MAX_EVENT_TYPE_BYTES + 1),
        ),
        Err(EventEnvelopeError::IdentityTooLong {
            field: "event_type",
            actual_bytes: MAX_EVENT_TYPE_BYTES + 1,
            max_bytes: MAX_EVENT_TYPE_BYTES,
        })
    );
}

#[test]
fn event_envelope_deserialization_cannot_bypass_byte_bounds() {
    let envelope = EventEnvelope::new(
        EventId::generate(),
        "ledger",
        "aggregate:1",
        1,
        "example.created",
        1,
        UserId::generate(),
        Utc::now(),
        CorrelationId::generate(),
        None,
    )
    .unwrap();
    let mut serialized = serde_json::to_value(envelope).unwrap();
    serialized["aggregate_id"] = serde_json::json!("a".repeat(MAX_EVENT_AGGREGATE_ID_BYTES + 1));

    assert!(serde_json::from_value::<EventEnvelope>(serialized).is_err());
}
