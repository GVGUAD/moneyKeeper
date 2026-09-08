use axum::Json;
use axum::extract::{Path, Query, State};

use crate::api::{ApiError, AuthenticatedUser};
use crate::contexts::reference_data::public::{
    CurrencyCatalog, CurrencyCatalogFacade, CurrencyError,
};
use crate::shared_kernel::CurrencyCode;

use super::dto::{CurrencyResponse, FxRateQuery};

pub(crate) async fn list(
    _user: AuthenticatedUser,
    State(catalog): State<CurrencyCatalogFacade>,
) -> Result<Json<Vec<CurrencyResponse>>, ApiError> {
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
) -> Result<Json<CurrencyResponse>, ApiError> {
    let code =
        CurrencyCode::new(code).map_err(|_| ApiError::bad_request("invalid currency code"))?;
    catalog
        .require_enabled(code)
        .await
        .map(|value| Json(value.into()))
        .map_err(map_error)
}

pub(crate) async fn fx_rate(
    _user: AuthenticatedUser,
    State(catalog): State<CurrencyCatalogFacade>,
    Query(query): Query<FxRateQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let base = CurrencyCode::new(query.base_currency)
        .map_err(|_| ApiError::bad_request("invalid base_currency"))?;
    let quote = CurrencyCode::new(query.quote_currency)
        .map_err(|_| ApiError::bad_request("invalid quote_currency"))?;
    match catalog.rate_as_of(base, quote, query.as_of).await {
        Ok(rate) => Ok(Json(serde_json::json!({"status":"available","rate":rate}))),
        Err(error) if error.is_not_found() => Ok(Json(
            serde_json::json!({"status":"missing","as_of":query.as_of}),
        )),
        Err(_) => Err(ApiError::internal(
            "reference_data.persistence",
            "FX rate query failed",
        )),
    }
}

fn map_error(error: CurrencyError) -> ApiError {
    if error.is_not_found() || error.is_disabled() {
        ApiError::not_found("currency was not found")
    } else {
        ApiError::internal(
            "reference_data.persistence",
            "currency storage operation failed",
        )
    }
}
