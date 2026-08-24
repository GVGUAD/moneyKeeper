//! Sharing application façade orchestration.

use super::{commands::*, ports::*, queries::*};
use crate::contexts::sharing::domain::SharingError;
use crate::contexts::sharing::domain::{BillSplitId, ContactId};
use crate::shared_kernel::UserId;
use std::sync::Arc;

#[derive(Clone)]
pub struct SharingFacade {
    contacts: Arc<dyn ContactRepository>,
    bills: Arc<dyn BillRepository>,
    settlements: Arc<dyn SettlementRepository>,
    accounting: Arc<dyn AccountingWorkflowRepository>,
}

impl SharingFacade {
    pub(crate) fn new(
        contacts: Arc<dyn ContactRepository>,
        bills: Arc<dyn BillRepository>,
        settlements: Arc<dyn SettlementRepository>,
        accounting: Arc<dyn AccountingWorkflowRepository>,
    ) -> Self {
        Self {
            contacts,
            bills,
            settlements,
            accounting,
        }
    }
    pub async fn create_contact(
        &self,
        command: CreateContact,
    ) -> Result<ContactResult, SharingError> {
        self.contacts.create_contact(command).await
    }
    pub async fn update_contact(
        &self,
        command: UpdateContact,
    ) -> Result<ContactResult, SharingError> {
        self.contacts.update_contact(command).await
    }
    pub async fn archive_contact(
        &self,
        command: ArchiveContact,
    ) -> Result<ContactResult, SharingError> {
        self.contacts.archive_contact(command).await
    }
    pub async fn contact(
        &self,
        user: UserId,
        id: ContactId,
    ) -> Result<Option<ContactView>, SharingError> {
        self.contacts.contact(user, id).await
    }
    pub async fn contacts(
        &self,
        user: UserId,
        include_archived: bool,
    ) -> Result<Vec<ContactView>, SharingError> {
        self.contacts.contacts(user, include_archived).await
    }
    pub async fn create_bill(&self, command: CreateBillSplit) -> Result<BillResult, SharingError> {
        self.bills.create_bill(command).await
    }
    pub async fn revise_bill(&self, command: ReviseBillSplit) -> Result<BillResult, SharingError> {
        self.bills.revise_bill(command).await
    }
    pub async fn cancel_bill(&self, command: CancelBillSplit) -> Result<BillResult, SharingError> {
        self.bills.cancel_bill(command).await
    }
    pub async fn bill(
        &self,
        user: UserId,
        id: BillSplitId,
    ) -> Result<Option<BillView>, SharingError> {
        self.bills.bill(user, id).await
    }
    pub async fn bills(&self, user: UserId) -> Result<Vec<BillView>, SharingError> {
        self.bills.bills(user).await
    }
    pub async fn create_settlement(
        &self,
        command: CreateSettlement,
    ) -> Result<SettlementResult, SharingError> {
        self.settlements.create_settlement(command).await
    }
    pub async fn settlements(
        &self,
        user: UserId,
        bill: BillSplitId,
    ) -> Result<Vec<SettlementView>, SharingError> {
        self.settlements.settlements(user, bill).await
    }
    pub async fn reverse_settlement(
        &self,
        command: ReverseSettlement,
    ) -> Result<SettlementResult, SharingError> {
        self.settlements.reverse_settlement(command).await
    }
    pub async fn complete_bill_accounting(
        &self,
        command: CompleteBillAccounting,
    ) -> Result<BillView, SharingError> {
        self.accounting.complete_bill_accounting(command).await
    }
    pub async fn complete_bill_cancellation(
        &self,
        command: CompleteBillCancellation,
    ) -> Result<BillView, SharingError> {
        self.accounting.complete_bill_cancellation(command).await
    }
    pub async fn complete_settlement_accounting(
        &self,
        command: CompleteSettlementAccounting,
    ) -> Result<SettlementView, SharingError> {
        self.accounting
            .complete_settlement_accounting(command)
            .await
    }
    pub async fn complete_settlement_reversal(
        &self,
        command: CompleteSettlementReversal,
    ) -> Result<SettlementView, SharingError> {
        self.accounting.complete_settlement_reversal(command).await
    }
    pub async fn claim_next_work(
        &self,
        holder: &str,
    ) -> Result<Option<SharingWorkflowWork>, SharingError> {
        self.accounting.claim_next_work(holder).await
    }
    pub async fn retry_work(&self, command: RetrySharingWorkflow) -> Result<(), SharingError> {
        self.accounting.retry_work(command).await
    }
    pub async fn fail_bill_accounting(
        &self,
        command: FailBillAccounting,
    ) -> Result<BillView, SharingError> {
        self.accounting.fail_bill_accounting(command).await
    }
    pub async fn fail_settlement_accounting(
        &self,
        command: FailSettlementAccounting,
    ) -> Result<SettlementView, SharingError> {
        self.accounting.fail_settlement_accounting(command).await
    }
}
