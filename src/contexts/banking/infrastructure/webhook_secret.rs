//! High-entropy callback credentials and keyed lookup digests.

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use rand::{RngCore, rngs::OsRng};
use sha2::{Digest, Sha256};

use crate::contexts::banking::application::{WebhookCredential, WebhookSecrets};

#[derive(Clone)]
pub struct WebhookSecretManager {
    lookup_key: [u8; 32],
}

impl WebhookSecretManager {
    pub const fn new(lookup_key: [u8; 32]) -> Self {
        Self { lookup_key }
    }
}

impl WebhookSecrets for WebhookSecretManager {
    fn generate(&self) -> WebhookCredential {
        let mut bytes = [0_u8; 32];
        OsRng.fill_bytes(&mut bytes);
        WebhookCredential::new(URL_SAFE_NO_PAD.encode(bytes))
            .expect("32 random bytes always form a valid callback credential")
    }
    fn digest(&self, credential: &WebhookCredential) -> [u8; 32] {
        let mut hash = Sha256::new();
        hash.update(self.lookup_key);
        hash.update(credential.expose().as_bytes());
        hash.finalize().into()
    }
    fn verify_digest(&self, actual: &[u8], expected: &[u8]) -> bool {
        constant_time_eq(actual, expected)
    }
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        })
        == 0
}
