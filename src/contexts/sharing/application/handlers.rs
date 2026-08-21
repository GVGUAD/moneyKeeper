//! Sharing application façade orchestration.

use super::{commands::*, queries::*};
use crate::contexts::sharing::domain::{BillSplitId, ContactId};
use crate::contexts::sharing::{domain::SharingError, infrastructure::PgSharingStore};
use crate::shared_kernel::UserId;

#[derive(Clone)]
pub struct SharingFacade {
    pub(crate) store: PgSharingStore,
}

impl SharingFacade {
    pub(crate) fn new(store: PgSharingStore) -> Self {
        Self { store }
    }
    pub async fn create_contact(
        &self,
        command: CreateContact,
    ) -> Result<ContactResult, SharingError> {
        self.store.create_contact(command).await
    }
    pub async fn update_contact(
        &self,
        command: UpdateContact,
    ) -> Result<ContactResult, SharingError> {
        self.store.update_contact(command).await
    }
    pub async fn archive_contact(
        &self,
        command: ArchiveContact,
    ) -> Result<ContactResult, SharingError> {
        self.store.archive_contact(command).await
    }
    pub async fn contact(
        &self,
        user: UserId,
        id: ContactId,
    ) -> Result<Option<ContactView>, SharingError> {
        self.store.contact(user, id).await
    }
    pub async fn contacts(
        &self,
        user: UserId,
        include_archived: bool,
    ) -> Result<Vec<ContactView>, SharingError> {
        self.store.contacts(user, include_archived).await
    }
    pub async fn create_bill(&self, command: CreateBillSplit) -> Result<BillResult, SharingError> {
        self.store.create_bill(command).await
    }
    pub async fn revise_bill(&self, command: ReviseBillSplit) -> Result<BillResult, SharingError> {
        self.store.revise_bill(command).await
    }
    pub async fn cancel_bill(&self, command: CancelBillSplit) -> Result<BillResult, SharingError> {
        self.store.cancel_bill(command).await
    }
    pub async fn bill(
        &self,
        user: UserId,
        id: BillSplitId,
    ) -> Result<Option<BillView>, SharingError> {
        self.store.bill(user, id).await
    }
    pub async fn bills(&self, user: UserId) -> Result<Vec<BillView>, SharingError> {
        self.store.bills(user).await
    }
    pub async fn create_settlement(
        &self,
        command: CreateSettlement,
    ) -> Result<SettlementResult, SharingError> {
        self.store.create_settlement(command).await
    }
    pub async fn reverse_settlement(
        &self,
        command: ReverseSettlement,
    ) -> Result<SettlementResult, SharingError> {
        self.store.reverse_settlement(command).await
    }
    pub async fn complete_bill_accounting(
        &self,
        command: CompleteBillAccounting,
    ) -> Result<BillView, SharingError> {
        self.store.complete_bill_accounting(command).await
    }
    pub async fn complete_bill_cancellation(
        &self,
        command: CompleteBillCancellation,
    ) -> Result<BillView, SharingError> {
        self.store.complete_bill_cancellation(command).await
    }
    pub async fn complete_settlement_accounting(
        &self,
        command: CompleteSettlementAccounting,
    ) -> Result<SettlementView, SharingError> {
        self.store.complete_settlement_accounting(command).await
    }
    pub async fn complete_settlement_reversal(
        &self,
        command: CompleteSettlementReversal,
    ) -> Result<SettlementView, SharingError> {
        self.store.complete_settlement_reversal(command).await
    }
}
