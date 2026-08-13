//! Strongly typed universal UUID identifiers.

/// Defines an opaque UUID-backed identifier.
///
/// Contexts use this macro for their own aggregate identifiers so unrelated
/// identifiers cannot be mixed accidentally.
#[macro_export]
macro_rules! define_uuid_id {
    ($(#[$meta:meta])* $visibility:vis $name:ident) => {
        $(#[$meta])*
        #[derive(
            Clone,
            Copy,
            Debug,
            PartialEq,
            Eq,
            PartialOrd,
            Ord,
            Hash,
            ::serde::Serialize,
            ::serde::Deserialize,
            ::sqlx::Type,
        )]
        #[serde(transparent)]
        #[sqlx(transparent)]
        $visibility struct $name(::uuid::Uuid);

        impl $name {
            /// Creates an identifier from an explicit UUID.
            pub const fn new(value: ::uuid::Uuid) -> Self {
                Self(value)
            }

            /// Creates an identifier from an explicit UUID.
            pub const fn from_uuid(value: ::uuid::Uuid) -> Self {
                Self::new(value)
            }

            /// Generates a random version-4 identifier.
            pub fn generate() -> Self {
                Self(::uuid::Uuid::new_v4())
            }

            /// Borrows the underlying UUID for persistence adapters.
            pub const fn as_uuid(&self) -> &::uuid::Uuid {
                &self.0
            }

            /// Returns the underlying UUID.
            pub const fn into_uuid(self) -> ::uuid::Uuid {
                self.0
            }
        }

        impl ::std::fmt::Display for $name {
            fn fmt(&self, formatter: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                ::std::fmt::Display::fmt(&self.0, formatter)
            }
        }

        impl ::std::str::FromStr for $name {
            type Err = ::uuid::Error;

            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                ::uuid::Uuid::parse_str(value).map(Self)
            }
        }

        impl ::std::convert::AsRef<::uuid::Uuid> for $name {
            fn as_ref(&self) -> &::uuid::Uuid {
                self.as_uuid()
            }
        }
    };
}

define_uuid_id!(
    /// Identifies the external Supabase user that owns financial data.
    pub UserId
);
define_uuid_id!(
    /// Identifies one immutable domain or integration event.
    pub EventId
);
define_uuid_id!(
    /// Correlates all work belonging to one cross-context workflow.
    pub CorrelationId
);
define_uuid_id!(
    /// Identifies the event or command that directly caused another event.
    pub CausationId
);
