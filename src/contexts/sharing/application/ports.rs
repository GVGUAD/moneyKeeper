//! Sharing persistence and Ledger anti-corruption boundaries.

use crate::contexts::sharing::application::{commands::*, queries::*};
use crate::contexts::sharing::domain::{BillSplitId, ContactId, SharingError};
use crate::shared_kernel::UserId;
use async_trait::async_trait;

#[async_trait]
pub(crate) trait ContactRepository: Send + Sync {
    async fn create_contact(&self, command: CreateContact) -> Result<ContactResult, SharingError>;
    async fn update_contact(&self, command: UpdateContact) -> Result<ContactResult, SharingError>;
    async fn archive_contact(&self, command: ArchiveContact)
    -> Result<ContactResult, SharingError>;
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
}

#[async_trait]
pub(crate) trait BillRepository: Send + Sync {
    async fn create_bill(&self, command: CreateBillSplit) -> Result<BillResult, SharingError>;
    async fn revise_bill(&self, command: ReviseBillSplit) -> Result<BillResult, SharingError>;
    async fn cancel_bill(&self, command: CancelBillSplit) -> Result<BillResult, SharingError>;
    async fn bill(
        &self,
        user_id: UserId,
        id: BillSplitId,
    ) -> Result<Option<BillView>, SharingError>;
    async fn bills(&self, user_id: UserId) -> Result<Vec<BillView>, SharingError>;
}

#[async_trait]
pub(crate) trait SettlementRepository: Send + Sync {
    async fn create_settlement(
        &self,
        command: CreateSettlement,
    ) -> Result<SettlementResult, SharingError>;
    async fn reverse_settlement(
        &self,
        command: ReverseSettlement,
    ) -> Result<SettlementResult, SharingError>;
}

#[async_trait]
pub(crate) trait AccountingWorkflowRepository: Send + Sync {
    async fn complete_bill_accounting(
        &self,
        command: CompleteBillAccounting,
    ) -> Result<BillView, SharingError>;
    async fn complete_bill_cancellation(
        &self,
        command: CompleteBillCancellation,
    ) -> Result<BillView, SharingError>;
    async fn complete_settlement_accounting(
        &self,
        command: CompleteSettlementAccounting,
    ) -> Result<SettlementView, SharingError>;
    async fn complete_settlement_reversal(
        &self,
        command: CompleteSettlementReversal,
    ) -> Result<SettlementView, SharingError>;
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
