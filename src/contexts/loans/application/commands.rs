//! Loan application commands and canonical request hashing.

use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::shared_kernel::UserId;

pub(crate) fn canonical_request_hash<T: Serialize>(
    scope: &str,
    target: &str,
    user: UserId,
    body: &T,
) -> Result<[u8; 32], serde_json::Error> {
    Ok(Sha256::digest(serde_json::to_vec(&(scope, target, user, body))?).into())
}
