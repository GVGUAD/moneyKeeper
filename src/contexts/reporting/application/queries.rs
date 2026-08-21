use super::super::public::{ReportRange, ReportResponse, ReportingFacade};
use crate::shared_kernel::UserId;
pub(crate) async fn read(
    f: &ReportingFacade,
    user: UserId,
    range: ReportRange,
    kind: &'static str,
) -> Result<ReportResponse, sqlx::Error> {
    f.store.read(user, range, kind).await
}
