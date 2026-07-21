use std::sync::Arc;

use jsonwebtoken::jwk::JwkSet;

use crate::application::accounts::AccountService;
use crate::application::categories::CategoryService;
use crate::application::monobank::MonobankService;
use crate::application::subscription_matching::MatchChargesUseCase;
use crate::application::subscriptions::SubscriptionService;
use crate::application::transactions::TransactionService;
use crate::application::user_settings::UserSettingsService;

#[derive(Clone)]
pub struct AppState {
    pub accounts: Arc<AccountService>,
    pub transactions: Arc<TransactionService>,
    pub categories: Arc<CategoryService>,
    pub monobank: Arc<MonobankService>,
    pub user_settings: Arc<UserSettingsService>,
    pub supabase_jwks: Arc<JwkSet>,
    pub subscriptions: Arc<SubscriptionService>,
    pub matcher: Arc<MatchChargesUseCase>,
    pub fx: Arc<dyn crate::domain::fx_rate::FxRateRepository>,
    pub gmail_oauth: Arc<crate::infrastructure::email::oauth::GmailOAuthService>,
}
