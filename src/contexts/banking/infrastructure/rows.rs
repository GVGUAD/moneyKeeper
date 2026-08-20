//! Persistence-only Banking row shapes.

pub(super) struct ConnectionRow {
    pub provider: String,
    pub state: String,
    pub active_credential_ciphertext: Option<Vec<u8>>,
    pub active_credential_nonce: Option<Vec<u8>>,
    pub active_credential_key_id: Option<String>,
    pub active_credential_envelope_version: Option<i16>,
    pub pending_credential_ciphertext: Option<Vec<u8>>,
    pub pending_credential_nonce: Option<Vec<u8>>,
    pub pending_credential_key_id: Option<String>,
    pub pending_credential_envelope_version: Option<i16>,
    pub credential_generation: i64,
}
