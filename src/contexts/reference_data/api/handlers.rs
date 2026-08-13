use axum::Json;
use axum::extract::{Path, State};

use crate::api::v2::{AuthenticatedUser, V2ApiError};
use crate::contexts::reference_data::public::{
    CurrencyCatalog, CurrencyCatalogFacade, CurrencyError,
};
use crate::shared_kernel::CurrencyCode;

use super::dto::CurrencyResponse;

pub(crate) async fn list(
    _user: AuthenticatedUser,
    State(catalog): State<CurrencyCatalogFacade>,
) -> Result<Json<Vec<CurrencyResponse>>, V2ApiError> {
    catalog
        .list_enabled()
        .await
        .map(|values| Json(values.into_iter().map(Into::into).collect()))
        .map_err(map_error)
}

pub(crate) async fn get(
    _user: AuthenticatedUser,
    State(catalog): State<CurrencyCatalogFacade>,
    Path(code): Path<String>,
) -> Result<Json<CurrencyResponse>, V2ApiError> {
    let code =
        CurrencyCode::new(code).map_err(|_| V2ApiError::bad_request("invalid currency code"))?;
    catalog
        .require_enabled(code)
        .await
        .map(|value| Json(value.into()))
        .map_err(map_error)
}

fn map_error(error: CurrencyError) -> V2ApiError {
    if error.is_not_found() || error.is_disabled() {
        V2ApiError::not_found("currency was not found")
    } else {
        V2ApiError::internal()
    }
}
