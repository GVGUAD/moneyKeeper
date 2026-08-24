//! Restart-safe sync orchestration over short fenced PostgreSQL claims.

use chrono::{DateTime, Utc};

use super::{
    BankingFacade, BeginSyncPage, CompleteSyncPage, RequestSyncJob, SyncJobView, SyncPageView,
};
use crate::contexts::banking::domain::BankingError;

impl BankingFacade {
    pub async fn request_sync_job(
        &self,
        command: RequestSyncJob,
    ) -> Result<SyncJobView, BankingError> {
        self.sync_jobs.request_sync_job(command).await
    }

    pub async fn claim_due_sync_job(
        &self,
        holder: &str,
        now: DateTime<Utc>,
        lease_seconds: i64,
    ) -> Result<Option<SyncJobView>, BankingError> {
        self.sync_jobs
            .claim_due_sync_job(holder, now, lease_seconds)
            .await
    }

    pub async fn begin_sync_page(
        &self,
        command: BeginSyncPage,
    ) -> Result<SyncPageView, BankingError> {
        self.sync_jobs.begin_sync_page(command).await
    }

    pub async fn complete_sync_page(
        &self,
        command: CompleteSyncPage,
    ) -> Result<SyncJobView, BankingError> {
        self.sync_jobs.complete_sync_page(command).await
    }

    pub async fn get_sync_job(
        &self,
        user_id: crate::shared_kernel::UserId,
        id: crate::contexts::banking::domain::SyncJobId,
    ) -> Result<SyncJobView, BankingError> {
        self.sync_jobs.get_sync_job(user_id, id).await
    }

    pub async fn list_sync_pages(
        &self,
        user_id: crate::shared_kernel::UserId,
        id: crate::contexts::banking::domain::SyncJobId,
    ) -> Result<Vec<SyncPageView>, BankingError> {
        self.sync_jobs.list_sync_pages(user_id, id).await
    }
}
