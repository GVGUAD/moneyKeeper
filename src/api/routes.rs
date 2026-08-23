//! Default unversioned Finance V2 router.

use std::sync::Arc;

use axum::Router;
use jsonwebtoken::jwk::JwkSet;

use crate::bootstrap::v2::SupportingContexts;

/// The exhaustive method/path manifest owned by the validated V2 composer.
pub use super::v2::ROUTE_MANIFEST;

/// Delegates to the single tested Finance V2 composition without rebuilding
/// a second route list.
pub fn router(contexts: SupportingContexts, jwks: Arc<JwkSet>) -> Router {
    super::v2::router(contexts, jwks)
}
