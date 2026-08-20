//! Read-only Ledger query facade and operational projection checks.

use super::super::{
    domain::{JournalEntryId, LedgerAccountId, LedgerError},
    public::{
        AccountView, ActivityCursor, JournalView, ProjectionMismatch,
        ProviderAccountBindingRejection, ProviderAccountBindingResult,
        ValidateProviderAccountBinding,
    },
};
use super::accounts::LedgerFacade;
use crate::shared_kernel::UserId;

impl LedgerFacade {
    /// Validates a provider mapping without revealing a cross-tenant account.
    pub async fn validate_provider_account_binding(
        &self,
        command: ValidateProviderAccountBinding,
    ) -> Result<ProviderAccountBindingResult, LedgerError> {
        let account = match self.queries.get_account(command.user_id, command.account_id).await {
            Ok(account) => account,
            Err(error) if error.is_not_found() => {
                return Ok(ProviderAccountBindingResult::Rejected(
                    ProviderAccountBindingRejection::NotFound,
                ));
            }
            Err(error) => return Err(error),
        };
        let rejection = if account.lifecycle == super::super::domain::AccountLifecycle::Archived {
            Some(ProviderAccountBindingRejection::Archived)
        } else if account.currency != command.currency {
            Some(ProviderAccountBindingRejection::CurrencyMismatch)
        } else if account.authority == super::super::domain::AccountAuthority::System {
            Some(ProviderAccountBindingRejection::SystemAccount)
        } else if account.kind != command.kind || account.nature != command.nature {
            Some(ProviderAccountBindingRejection::IncompatibleKindOrNature)
        } else {
            None
        };
        Ok(match rejection {
            Some(reason) => ProviderAccountBindingResult::Rejected(reason),
            None => ProviderAccountBindingResult::Accepted(account),
        })
    }

    /// Lists tenant-visible accounts including archived history and balances.
    pub async fn list_accounts(&self, user_id: UserId) -> Result<Vec<AccountView>, LedgerError> {
        self.queries.list_accounts(user_id).await
    }

    /// Gets one tenant-scoped account balance view.
    pub async fn get_account(
        &self,
        user_id: UserId,
        id: LedgerAccountId,
    ) -> Result<AccountView, LedgerError> {
        self.queries.get_account(user_id, id).await
    }

    /// Lists immutable journal activity in stable reverse chronological order.
    pub async fn account_activity(
        &self,
        user_id: UserId,
        account_id: LedgerAccountId,
        after: Option<ActivityCursor>,
        limit: u32,
    ) -> Result<Vec<JournalView>, LedgerError> {
        self.queries
            .account_activity(user_id, account_id, after, limit)
            .await
    }

    /// Lists tenant journal facts in stable reverse chronological order.
    pub async fn list_journals(
        &self,
        user_id: UserId,
        after: Option<ActivityCursor>,
        limit: u32,
    ) -> Result<Vec<JournalView>, LedgerError> {
        self.queries.list_journals(user_id, after, limit).await
    }

    /// Gets one fully detailed immutable journal.
    pub async fn get_journal(
        &self,
        user_id: UserId,
        id: JournalEntryId,
    ) -> Result<JournalView, LedgerError> {
        self.queries.get_journal(user_id, id).await
    }

    /// Detects projection differences without mutating data.
    pub async fn verify_projection(&self) -> Result<Vec<ProjectionMismatch>, LedgerError> {
        self.projection.verify().await
    }

    /// Operationally rebuilds all balances from immutable postings.
    pub async fn rebuild_projection(&self) -> Result<(), LedgerError> {
        self.projection.rebuild().await
    }
}
