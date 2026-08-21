use crate::contexts::reporting::public::{ReportRange, ReportResponse};
use crate::shared_kernel::UserId;
use std::future::Future;
pub(crate) trait ReadReport: Send + Sync {
    fn read(
        &self,
        user: UserId,
        range: ReportRange,
        kind: &'static str,
    ) -> impl Future<Output = Result<ReportResponse, sqlx::Error>> + Send;
}
