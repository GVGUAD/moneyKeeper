use super::dto::ReportQuery;
use crate::{
    api::v2::{AuthenticatedUser, V2ApiError},
    contexts::reporting::{
        application,
        public::{ReportResponse, ReportingFacade},
    },
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
) -> Result<Json<ReportResponse>, V2ApiError> {
    let range = q
        .try_into()
        .map_err(|_| V2ApiError::bad_request("invalid report range"))?;
    application::queries::read(&f, user, range, kind)
        .await
        .map(Json)
        .map_err(|_| V2ApiError::internal())
}
macro_rules! handler {
    ($name:ident,$kind:literal) => {
        pub(crate) async fn $name(
            State(f): State<ReportingFacade>,
            AuthenticatedUser(user): AuthenticatedUser,
            Query(q): Query<ReportQuery>,
        ) -> Result<Json<ReportResponse>, V2ApiError> {
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
