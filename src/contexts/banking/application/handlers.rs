//! Banking command/query facade with private adapters.

use std::sync::Arc;

use super::ports::{CredentialCipher, ProviderClient};
use super::{ConnectProvider, ConnectionResult, ProviderConnectionView, ReplaceProviderCredential};
use crate::contexts::banking::domain::BankingError;
use crate::contexts::banking::infrastructure::PgBankingStore;
use crate::shared_kernel::UserId;

#[derive(Clone)]
pub struct BankingFacade {
    pub(crate) store: PgBankingStore,
    pub(crate) cipher: Arc<dyn CredentialCipher>,
    pub(crate) provider: Arc<dyn ProviderClient>,
}

impl BankingFacade {
    pub(crate) fn new(store: PgBankingStore, cipher: Arc<dyn CredentialCipher>, provider: Arc<dyn ProviderClient>) -> Self { Self { store, cipher, provider } }

    pub async fn connect_provider(&self, command: ConnectProvider) -> Result<ConnectionResult, BankingError> {
        self.store.connect(command, self.cipher.as_ref()).await
    }

    pub async fn replace_provider_credential(&self, command: ReplaceProviderCredential) -> Result<ConnectionResult, BankingError> {
        self.store.replace_credential(command, self.cipher.as_ref()).await
    }

    pub async fn validate_and_discover(&self, user_id: UserId, connection_id: super::super::domain::ProviderConnectionId) -> Result<Vec<super::super::infrastructure::NormalizedResource>, BankingError> {
        self.store.validate_and_discover(user_id, connection_id, self.cipher.as_ref(), self.provider.as_ref()).await
    }

    pub async fn list_connections(&self, user_id: UserId) -> Result<Vec<ProviderConnectionView>, BankingError> { self.store.list_connections(user_id).await }
}
