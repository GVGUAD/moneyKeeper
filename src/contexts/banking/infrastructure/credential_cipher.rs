//! AES-256-GCM credential envelopes bound to Banking aggregate identity.

use aes_gcm::{
    Aes256Gcm, KeyInit, Nonce,
    aead::{Aead, Payload},
};
use rand::{RngCore, rngs::OsRng};

use super::super::{
    application::{CredentialBinding, CredentialCipher, ProviderCredential},
    domain::{BankingError, CredentialEnvelope},
};

#[derive(Clone)]
pub struct Aes256CredentialCipher {
    key_id: String,
    key: [u8; 32],
}

impl Aes256CredentialCipher {
    pub fn new(key_id: impl Into<String>, key: [u8; 32]) -> Result<Self, BankingError> {
        let key_id = key_id.into();
        if key_id.is_empty() || key_id.len() > 100 {
            return Err(BankingError::InvalidValue("invalid encryption key id"));
        }
        Ok(Self { key_id, key })
    }
}

impl CredentialCipher for Aes256CredentialCipher {
    fn encrypt(
        &self,
        credential: &ProviderCredential,
        binding: &CredentialBinding,
    ) -> Result<CredentialEnvelope, BankingError> {
        let cipher = Aes256Gcm::new_from_slice(&self.key)
            .map_err(|_| BankingError::CredentialUnavailable)?;
        let mut nonce_bytes = [0_u8; 12];
        OsRng.fill_bytes(&mut nonce_bytes);
        let ciphertext = cipher
            .encrypt(
                Nonce::from_slice(&nonce_bytes),
                Payload {
                    msg: credential.expose().as_bytes(),
                    aad: &binding.associated_data(),
                },
            )
            .map_err(|_| BankingError::CredentialUnavailable)?;
        CredentialEnvelope::new(self.key_id.clone(), nonce_bytes.to_vec(), ciphertext)
    }

    fn decrypt(
        &self,
        envelope: &CredentialEnvelope,
        binding: &CredentialBinding,
    ) -> Result<ProviderCredential, BankingError> {
        if envelope.key_id() != self.key_id
            || envelope.envelope_version() != 1
            || envelope.nonce().len() != 12
        {
            return Err(BankingError::CredentialUnavailable);
        }
        let cipher = Aes256Gcm::new_from_slice(&self.key)
            .map_err(|_| BankingError::CredentialUnavailable)?;
        let plaintext = cipher
            .decrypt(
                Nonce::from_slice(envelope.nonce()),
                Payload {
                    msg: envelope.ciphertext(),
                    aad: &binding.associated_data(),
                },
            )
            .map_err(|_| BankingError::CredentialUnavailable)?;
        let value =
            String::from_utf8(plaintext).map_err(|_| BankingError::CredentialUnavailable)?;
        ProviderCredential::new(value)
    }
}
