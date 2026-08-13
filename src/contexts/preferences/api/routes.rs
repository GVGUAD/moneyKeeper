use axum::Router;
use axum::routing::get;

use crate::contexts::preferences::public::PreferencesFacade;
use crate::contexts::reference_data::public::CurrencyCatalogFacade;

use super::handlers::{self, PreferencesApiState};

pub(crate) fn router(preferences: PreferencesFacade, currencies: CurrencyCatalogFacade) -> Router {
    Router::new()
        .route("/preferences", get(handlers::get).patch(handlers::update))
        .with_state(PreferencesApiState {
            preferences,
            currencies,
        })
}
