use super::dto::ReportQuery;
use crate::{
    api::{ApiError, AuthenticatedUser},
    contexts::reporting::public::{ReportResponse, ReportingFacade},
};
use axum::{
    Json,
    extract::{Query, State},
};
async fn report(
    f: ReportingFacade,
    user: crate::shared_kernel::UserId,
    q: ReportQuery,
    kind: &'static str,
) -> Result<Json<ReportResponse>, ApiError> {
    let range = q
        .try_into()
        .map_err(|_| ApiError::bad_request("invalid report range"))?;
    f.read(user, range, kind)
        .await
        .map(Json)
        .map_err(|_| ApiError::internal("reporting.persistence", "report query failed"))
}
macro_rules! handler {
    ($name:ident,$kind:literal) => {
        pub(crate) async fn $name(
            State(f): State<ReportingFacade>,
            AuthenticatedUser(user): AuthenticatedUser,
            Query(q): Query<ReportQuery>,
        ) -> Result<Json<ReportResponse>, ApiError> {
            report(f, user, q, $kind).await
        }
    };
}
handler!(balance_history, "balance_history");
handler!(cashflow, "cashflow");
handler!(spending, "spending");
handler!(liabilities, "liabilities");
handler!(reconciliations, "reconciliations");
handler!(recurring, "recurring");
handler!(net_worth, "net_worth");
