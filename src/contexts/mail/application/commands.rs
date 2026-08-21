//! Canonical Mail command receipts.
use crate::shared_kernel::UserId;
use serde::Serialize;
use sha2::{Digest, Sha256};
pub(crate) fn canonical_request_hash<T: Serialize>(
    scope: &str,
    target: Option<&str>,
    user_id: UserId,
    body: &T,
) -> Result<[u8; 32], serde_json::Error> {
    let canonical = serde_json::to_vec(&(scope, target, user_id, body))?;
    Ok(Sha256::digest(canonical).into())
}
