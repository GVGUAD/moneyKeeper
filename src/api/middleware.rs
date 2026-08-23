//! Shared HTTP identity inserted by the centralized authentication boundary.

use uuid::Uuid;

/// Authenticated tenant identity.
#[derive(Clone, Debug)]
pub struct AuthUser(pub Uuid);
