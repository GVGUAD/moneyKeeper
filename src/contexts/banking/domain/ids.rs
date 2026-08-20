//! Strong Banking-owned identities.

use crate::define_uuid_id;

define_uuid_id!(
    /// Identifies one provider connection.
    pub ProviderConnectionId
);
define_uuid_id!(
    /// Identifies one external provider resource.
    pub ExternalResourceId
);
define_uuid_id!(
    /// Identifies one retained provider event revision.
    pub ProviderEventId
);
define_uuid_id!(
    /// Identifies one durable sync job.
    pub SyncJobId
);
define_uuid_id!(
    /// Identifies one immutable provider balance observation.
    pub BalanceObservationId
);
define_uuid_id!(
    /// Identifies one historical resource mapping.
    pub ResourceMappingId
);
