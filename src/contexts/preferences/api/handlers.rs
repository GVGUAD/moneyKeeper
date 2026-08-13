use axum::Json;
use axum::extract::State;
use chrono::Utc;

use crate::api::v2::{AuthenticatedUser, V2ApiError, V2Json};
use crate::contexts::preferences::public::{Preferences, PreferencesError, PreferencesFacade};
use crate::contexts::reference_data::public::CurrencyCatalogFacade;
use crate::shared_kernel::CurrencyCode;

use super::dto::{PreferencesResponse, UpdatePreferencesRequest};

#[derive(Clone)]
pub(crate) struct PreferencesApiState {
    pub(crate) preferences: PreferencesFacade,
    pub(crate) currencies: CurrencyCatalogFacade,
}

pub(crate) async fn get(
    AuthenticatedUser(user_id): AuthenticatedUser,
    State(state): State<PreferencesApiState>,
) -> Result<Json<PreferencesResponse>, V2ApiError> {
    state
        .preferences
        .get(user_id, Utc::now())
        .await
        .map(|value| Json(value.into()))
        .map_err(map_error)
}

pub(crate) async fn update(
    AuthenticatedUser(user_id): AuthenticatedUser,
    State(state): State<PreferencesApiState>,
    V2Json(request): V2Json<UpdatePreferencesRequest>,
) -> Result<Json<PreferencesResponse>, V2ApiError> {
    let expected_version = validate_expected_version(request.expected_version)?;
    let currency = CurrencyCode::new(request.base_currency)
        .map_err(|_| V2ApiError::bad_request("invalid currency code"))?;
    state
        .preferences
        .set_base_currency(
            &state.currencies,
            user_id,
            currency,
            expected_version,
            Utc::now(),
        )
        .await
        .map(|value| Json(value.into()))
        .map_err(map_error)
}

fn map_error(error: PreferencesError) -> V2ApiError {
    if error.is_currency_rejected() {
        V2ApiError::bad_request("base currency is not enabled")
    } else if error.is_version_conflict() {
        V2ApiError::conflict("preferences version conflict")
    } else {
        V2ApiError::internal()
    }
}

fn validate_expected_version(expected_version: i64) -> Result<i64, V2ApiError> {
    if expected_version < 0 {
        return Err(V2ApiError::bad_request(
            "expected_version must be non-negative",
        ));
    }
    Ok(expected_version)
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use axum::response::IntoResponse;

    use super::validate_expected_version;

    #[test]
    fn preference_versions_match_the_http_contract() {
        assert_eq!(validate_expected_version(0).unwrap(), 0);
        let response = validate_expected_version(-1)
            .expect_err("negative versions are invalid")
            .into_response();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
}
