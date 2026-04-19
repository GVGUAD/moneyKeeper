use std::sync::Arc;

use jsonwebtoken::jwk::JwkSet;

use crate::application::accounts::AccountService;
use crate::application::categories::CategoryService;
use crate::application::monobank::MonobankService;
use crate::application::transactions::TransactionService;

#[derive(Clone)]
pub struct  AppState {
    pub accounts: Arc<AccountService>,
    pub transactions: Arc<TransactionService>,
    pub categories: Arc<CategoryService>,
    pub monobank: Arc<MonobankService>,
    pub supabase_jwks: Arc<JwkSet>,
}
