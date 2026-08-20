use async_trait::async_trait;

use crate::contexts::banking::application::{ProviderClient, ProviderCredential, ProviderFailure};

#[derive(Clone)]
pub struct MonobankClient { client: reqwest::Client, base_url: String }

impl MonobankClient {
    pub fn new(base_url: impl Into<String>) -> Self { Self { client: reqwest::Client::new(), base_url: base_url.into() } }
}

#[async_trait]
impl ProviderClient for MonobankClient {
    async fn client_info(&self, credential: &ProviderCredential) -> Result<String, ProviderFailure> {
        let response = self.client.get(format!("{}/personal/client-info", self.base_url.trim_end_matches('/'))).header("X-Token", credential.expose()).send().await.map_err(|_| ProviderFailure::Classified { class: crate::contexts::banking::application::ProviderFailureClass::Transient })?;
        if !response.status().is_success() { return Err(ProviderFailure::Classified { class: super::MonobankAdapter::classify_status(response.status().as_u16()) }); }
        response.text().await.map_err(|_| ProviderFailure::InvalidResponse)
    }
}
