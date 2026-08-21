//! Sharing persistence and Ledger anti-corruption boundaries.

use crate::contexts::sharing::application::queries::{BillView, ContactView};
use crate::contexts::sharing::domain::{BillSplitId, ContactId, SharingError};
use crate::shared_kernel::UserId;
use async_trait::async_trait;

#[async_trait]
pub trait SharingQueries: Send + Sync {
    async fn contact(
        &self,
        user_id: UserId,
        id: ContactId,
    ) -> Result<Option<ContactView>, SharingError>;
    async fn contacts(
        &self,
        user_id: UserId,
        include_archived: bool,
    ) -> Result<Vec<ContactView>, SharingError>;
    async fn bill(
        &self,
        user_id: UserId,
        id: BillSplitId,
    ) -> Result<Option<BillView>, SharingError>;
    async fn bills(&self, user_id: UserId) -> Result<Vec<BillView>, SharingError>;
}

/// Opaque Ledger capabilities required by Sharing process managers.
#[async_trait]
pub trait SharingLedgerPort: Send + Sync {
    async fn account_bill(&self, bill: &BillView) -> Result<Option<uuid::Uuid>, SharingError>;
    async fn reverse_bill(
        &self,
        user_id: UserId,
        bill_id: BillSplitId,
        revision: u32,
    ) -> Result<Option<uuid::Uuid>, SharingError>;
}
