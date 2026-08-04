use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::Decimal;
use uuid::Uuid;

use crate::application::categories::CategoryService;
#[cfg(test)]
use crate::domain::email::RawEmail;
use crate::domain::email::{EmailFetchBatch, EmailFetcher};
#[cfg(test)]
use crate::domain::email_connection::EmailProvider;
use crate::domain::email_connection::{
    EmailConnection, EmailConnectionRepository, EmailConnectionStatus,
};
use crate::domain::email_sync::{
    EmailMessageFailure, EmailSyncRepository, MessageIngestionOutcome, RecurringReceiptIngestion,
    SyncLeaseClaim,
};
use crate::domain::fx_rate::FxRateRepository;
#[cfg(test)]
use crate::domain::receipt_parser::ParsedReceipt;
use crate::domain::subscription::{
    BillingPeriod, MarkTransactionSubscription, MarkTransactionSubscriptionOutcome, Subscription,
    SubscriptionListFilter, SubscriptionRepository, SubscriptionStatus,
    TransactionSubscriptionTarget,
};
#[cfg(test)]
use crate::domain::subscription_charge::{ChargeMatchStatus, ChargeSource, ReceiptKind};
use crate::domain::subscription_charge::{
    SubscriptionCharge, SubscriptionChargeRepository, TransactionSubscriptionLink,
};
use crate::domain::subscription_error::SubscriptionError;
use crate::infrastructure::email::oauth::{GmailOAuthClient, GmailProviderError};
use crate::infrastructure::email::parsers::ParserRegistry;

pub struct Forecast {
    pub window_start: DateTime<Utc>,
    pub window_end: DateTime<Utc>,
    pub base_currency: String,
    pub base_total: Decimal,
    pub by_currency: HashMap<String, Decimal>,
    pub monthly_equivalent_total: Decimal,
    pub yearly_equivalent_total: Decimal,
    pub normalized_by_currency: HashMap<String, NormalizedCurrencyTotals>,
    pub fx_quotes: Vec<ForecastFxQuote>,
    pub complete: bool,
    pub warnings: Vec<String>,
}

pub struct NormalizedCurrencyTotals {
    pub monthly: Decimal,
    pub yearly: Decimal,
}

pub struct ForecastFxQuote {
    pub from_currency: String,
    pub to_currency: String,
    pub rate: Decimal,
    pub requested_date: NaiveDate,
    pub rate_date: NaiveDate,
}

pub struct MarkTransactionSubscriptionResult {
    pub subscription: Subscription,
    pub charge: SubscriptionCharge,
    pub subscription_created: bool,
    pub already_linked: bool,
}

#[cfg(test)]
pub struct ConnectGmailParams {
    pub email_address: String,
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: DateTime<Utc>,
}

/// Opaque, already-leased sync job. Handlers can return `202 Accepted` after
/// claiming it without gaining access to repositories or provider secrets.
pub struct ClaimedEmailSync {
    connection: EmailConnection,
    owner: Uuid,
    manual: bool,
}

pub struct SubscriptionService {
    connections: Arc<dyn EmailConnectionRepository>,
    subscriptions: Arc<dyn SubscriptionRepository>,
    charges: Arc<dyn SubscriptionChargeRepository>,
    fetcher: Arc<dyn EmailFetcher>,
    parsers: Arc<ParserRegistry>,
    sync: Option<Arc<dyn EmailSyncRepository>>,
    oauth_client: Option<Arc<dyn GmailOAuthClient>>,
    categories: Option<Arc<CategoryService>>,
}

impl SubscriptionService {
    pub fn new(
        connections: Arc<dyn EmailConnectionRepository>,
        subscriptions: Arc<dyn SubscriptionRepository>,
        charges: Arc<dyn SubscriptionChargeRepository>,
        fetcher: Arc<dyn EmailFetcher>,
        parsers: Arc<ParserRegistry>,
    ) -> Self {
        Self {
            connections,
            subscriptions,
            charges,
            fetcher,
            parsers,
            sync: None,
            oauth_client: None,
            categories: None,
        }
    }

    pub fn with_reliable_sync(
        mut self,
        sync: Arc<dyn EmailSyncRepository>,
        oauth_client: Arc<dyn GmailOAuthClient>,
    ) -> Self {
        self.sync = Some(sync);
        self.oauth_client = Some(oauth_client);
        self
    }

    pub fn with_category_validation(mut self, categories: Arc<CategoryService>) -> Self {
        self.categories = Some(categories);
        self
    }

    #[cfg(test)]
    pub async fn connect_gmail(
        &self,
        user_id: Uuid,
        params: ConnectGmailParams,
    ) -> anyhow::Result<EmailConnection> {
        let conn = EmailConnection {
            id: Uuid::new_v4(),
            user_id,
            provider: EmailProvider::Gmail,
            email_address: params.email_address,
            oauth_access_token: params.access_token.into(),
            oauth_refresh_token: params.refresh_token.into(),
            credential_version: 0,
            access_token_expires_at: params.expires_at,
            status: EmailConnectionStatus::Connected,
            last_synced_at: None,
            last_history_id: None,
            created_at: Utc::now(),
        };
        self.connections.create(&conn).await?;
        Ok(conn)
    }

    pub async fn list_connections(&self, user_id: Uuid) -> anyhow::Result<Vec<EmailConnection>> {
        self.connections.list_by_user(user_id).await
    }

    pub async fn delete_connection(&self, id: Uuid, user_id: Uuid) -> anyhow::Result<()> {
        let exists = self.connections.find_by_id(id, user_id).await?.is_some();
        if !exists {
            return Err(SubscriptionError::ConnectionNotFound.into());
        }
        self.connections.delete(id, user_id).await
    }

    /// Sync one connection. Returns newly-inserted charge ids.
    pub async fn sync_connection(&self, conn_id: Uuid) -> anyhow::Result<Vec<Uuid>> {
        self.sync_connection_inner(conn_id, None, false).await
    }

    pub async fn sync_connection_for_user(
        &self,
        conn_id: Uuid,
        user_id: Uuid,
        manual: bool,
    ) -> anyhow::Result<Vec<Uuid>> {
        self.sync_connection_inner(conn_id, Some(user_id), manual)
            .await
    }

    /// Validate ownership and atomically acquire the same database lease used
    /// by schedulers. The caller may then run the opaque job in the background.
    pub async fn claim_connection_for_user(
        &self,
        conn_id: Uuid,
        user_id: Uuid,
    ) -> anyhow::Result<ClaimedEmailSync> {
        let connection = self
            .connections
            .find_by_id(conn_id, user_id)
            .await?
            .filter(|connection| connection.status == EmailConnectionStatus::Connected)
            .ok_or(SubscriptionError::ConnectionNotFound)?;
        self.claim_resolved_connection(connection, true).await
    }

    pub async fn run_claimed_connection(&self, job: ClaimedEmailSync) -> anyhow::Result<Vec<Uuid>> {
        let sync = self
            .sync
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("reliable email sync is not configured"))?;
        let mut connection = job.connection.clone();
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(8 * 60),
            self.run_claimed_sync(&mut connection, job.manual),
        )
        .await;
        match result {
            Ok(Ok((new_charge_ids, history_id))) => {
                let completed = sync
                    .complete_connection(
                        job.connection.id,
                        job.owner,
                        connection.credential_version,
                        Utc::now(),
                        history_id,
                        Utc::now() + chrono::Duration::hours(1),
                    )
                    .await?;
                if !completed {
                    anyhow::bail!("email sync lease was lost before cursor completion");
                }
                Ok(new_charge_ids)
            }
            Ok(Err(error)) => {
                let reconnect_required = is_invalid_credentials(&error);
                let error_kind = if reconnect_required {
                    "invalid_credentials"
                } else {
                    "transient_sync_failure"
                };
                let _ = sync
                    .fail_connection(
                        job.connection.id,
                        job.owner,
                        connection.credential_version,
                        error_kind,
                        Utc::now(),
                        reconnect_required,
                    )
                    .await;
                Err(error)
            }
            Err(_) => {
                let _ = sync
                    .fail_connection(
                        job.connection.id,
                        job.owner,
                        connection.credential_version,
                        "sync_deadline",
                        Utc::now(),
                        false,
                    )
                    .await;
                anyhow::bail!("email connection sync exceeded the eight-minute deadline")
            }
        }
    }

    async fn sync_connection_inner(
        &self,
        conn_id: Uuid,
        expected_user_id: Option<Uuid>,
        manual: bool,
    ) -> anyhow::Result<Vec<Uuid>> {
        let conn = if let Some(user_id) = expected_user_id {
            self.connections
                .find_by_id(conn_id, user_id)
                .await?
                .filter(|connection| connection.status == EmailConnectionStatus::Connected)
        } else {
            self.connections
                .list_connected()
                .await?
                .into_iter()
                .find(|connection| connection.id == conn_id)
        }
        .ok_or(SubscriptionError::ConnectionNotFound)?;

        if self.sync.is_none() {
            #[cfg(test)]
            {
                return self.sync_without_ledger(conn).await;
            }
            #[cfg(not(test))]
            anyhow::bail!("reliable email sync is not configured");
        }
        let job = self.claim_resolved_connection(conn, manual).await?;
        self.run_claimed_connection(job).await
    }

    async fn claim_resolved_connection(
        &self,
        connection: EmailConnection,
        manual: bool,
    ) -> anyhow::Result<ClaimedEmailSync> {
        let sync = self
            .sync
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("reliable email sync is not configured"))?;
        let owner = Uuid::new_v4();
        let now = Utc::now();
        match sync
            .claim_connection(
                connection.id,
                owner,
                now,
                now + chrono::Duration::minutes(10),
            )
            .await?
        {
            SyncLeaseClaim::Acquired => {}
            SyncLeaseClaim::Busy => return Err(SubscriptionError::SyncInProgress.into()),
            SyncLeaseClaim::NotFound => return Err(SubscriptionError::ConnectionNotFound.into()),
        }

        if manual
            && let Err(error) = sync
                .requeue_for_manual_resync(connection.id, connection.user_id, now)
                .await
        {
            let _ = sync
                .fail_connection(
                    connection.id,
                    owner,
                    connection.credential_version,
                    "manual_requeue_failure",
                    Utc::now(),
                    false,
                )
                .await;
            return Err(error);
        }
        Ok(ClaimedEmailSync {
            connection,
            owner,
            manual,
        })
    }

    async fn run_claimed_sync(
        &self,
        conn: &mut EmailConnection,
        _manual: bool,
    ) -> anyhow::Result<(Vec<Uuid>, Option<String>)> {
        let sync = self
            .sync
            .as_ref()
            .expect("reliable sync repository checked by caller");
        if conn.access_token_expires_at <= Utc::now() + chrono::Duration::minutes(1) {
            let client = self
                .oauth_client
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("Gmail OAuth refresh client is not configured"))?;
            if conn.oauth_refresh_token.trim().is_empty() {
                anyhow::bail!("Gmail refresh token is missing");
            }
            let refreshed = client.refresh(&conn.oauth_refresh_token).await?;
            let access_token = refreshed.access_token;
            let refresh_token = refreshed
                .refresh_token
                .filter(|secret| !secret.trim().is_empty())
                .unwrap_or_else(|| conn.oauth_refresh_token.clone());
            let updated = self
                .connections
                .update_tokens(
                    conn.id,
                    conn.credential_version,
                    access_token.expose(),
                    refresh_token.expose(),
                    refreshed.expires_at,
                )
                .await?;
            if !updated {
                anyhow::bail!("email connection credentials changed during sync");
            }
            conn.oauth_access_token = access_token;
            conn.oauth_refresh_token = refresh_token;
            conn.credential_version += 1;
            conn.access_token_expires_at = refreshed.expires_at;
        }

        let retryable = sync
            .list_retryable_messages(conn.id, Utc::now(), 100)
            .await?;
        let retry_batch = if retryable.is_empty() {
            EmailFetchBatch::default()
        } else {
            self.fetcher
                .fetch_by_ids(
                    conn,
                    retryable
                        .into_iter()
                        .map(|message| message.provider_message_id)
                        .collect(),
                )
                .await?
        };
        let batch = self.fetcher.fetch_new(conn).await?;
        let history_id = batch.next_history_id.clone();

        let mut seen = HashSet::new();
        let mut emails = retry_batch
            .emails
            .into_iter()
            .chain(batch.emails)
            .filter(|email| seen.insert(email.provider_message_id.clone()))
            .collect::<Vec<_>>();
        emails.sort_by_key(|email| email.received_at);

        for failure in retry_batch.failures.into_iter().chain(batch.failures) {
            let now = Utc::now();
            sync.record_failure(&EmailMessageFailure {
                connection_id: conn.id,
                user_id: conn.user_id,
                provider_message_id: failure.provider_message_id,
                rfc_message_id: None,
                received_at: now,
                error_kind: failure.error_kind,
                recorded_at: now,
            })
            .await?;
        }

        let mut ignored_ids = HashSet::new();
        for provider_message_id in retry_batch
            .ignored_message_ids
            .into_iter()
            .chain(batch.ignored_message_ids)
            .filter(|message_id| ignored_ids.insert(message_id.clone()))
        {
            let now = Utc::now();
            sync.record_ignored(
                conn.id,
                conn.user_id,
                &provider_message_id,
                None,
                now,
                "untrusted_sender_authentication",
            )
            .await?;
        }

        let mut new_charge_ids = Vec::new();
        for email in emails {
            let Some(parser) = self.parsers.find(&email.from) else {
                sync.record_ignored(
                    conn.id,
                    conn.user_id,
                    &email.provider_message_id,
                    email.rfc_message_id.as_deref(),
                    email.received_at,
                    "unsupported_sender",
                )
                .await?;
                continue;
            };
            let receipt = match parser.parse(&email) {
                Ok(Some(receipt)) => receipt,
                Ok(None) => {
                    sync.record_ignored(
                        conn.id,
                        conn.user_id,
                        &email.provider_message_id,
                        email.rfc_message_id.as_deref(),
                        email.received_at,
                        "not_recurring",
                    )
                    .await?;
                    continue;
                }
                Err(_error) => {
                    sync.record_failure(&EmailMessageFailure {
                        connection_id: conn.id,
                        user_id: conn.user_id,
                        provider_message_id: email.provider_message_id.clone(),
                        rfc_message_id: email.rfc_message_id.clone(),
                        received_at: email.received_at,
                        error_kind: "parser_failure".to_string(),
                        recorded_at: Utc::now(),
                    })
                    .await?;
                    continue;
                }
            };
            if receipt.amount <= Decimal::ZERO || receipt.currency.trim().is_empty() {
                sync.record_ignored(
                    conn.id,
                    conn.user_id,
                    &email.provider_message_id,
                    email.rfc_message_id.as_deref(),
                    email.received_at,
                    "invalid_recurring_amount_or_currency",
                )
                .await?;
                continue;
            }
            let Some(billing_period) = receipt.billing_period_hint else {
                sync.record_ignored(
                    conn.id,
                    conn.user_id,
                    &email.provider_message_id,
                    email.rfc_message_id.as_deref(),
                    email.received_at,
                    "recurrence_unknown",
                )
                .await?;
                continue;
            };
            let outcome = sync
                .ingest_recurring(&RecurringReceiptIngestion {
                    connection_id: conn.id,
                    user_id: conn.user_id,
                    provider_message_id: email.provider_message_id,
                    rfc_message_id: email.rfc_message_id,
                    received_at: email.received_at,
                    receipt,
                    billing_period,
                })
                .await?;
            if let MessageIngestionOutcome::ChargeCreated(id) = outcome {
                new_charge_ids.push(id);
            }
        }

        Ok((new_charge_ids, history_id))
    }

    #[cfg(test)]
    async fn sync_without_ledger(&self, conn: EmailConnection) -> anyhow::Result<Vec<Uuid>> {
        let batch = self.fetcher.fetch_new(&conn).await?;
        if !batch.failures.is_empty() {
            anyhow::bail!(
                "email fetch returned message failures but no durable sync ledger is configured"
            );
        }
        let mut emails = batch.emails;
        emails.sort_by_key(|email| email.received_at);
        let mut new_charge_ids = Vec::new();
        for email in emails {
            let Some(parser) = self.parsers.find(&email.from) else {
                continue;
            };
            let Some(receipt) = parser.parse(&email)? else {
                continue;
            };
            if receipt.amount <= Decimal::ZERO || receipt.currency.trim().is_empty() {
                continue;
            }
            let Some(upsert) = self
                .upsert_subscription_from_receipt(conn.user_id, &receipt)
                .await?
            else {
                continue;
            };
            let sub = upsert.subscription;
            let source_key = format!("gmail:{}:{}", conn.id, email.provider_message_id);
            let now = Utc::now();
            let charge = SubscriptionCharge {
                id: Uuid::new_v4(),
                subscription_id: sub.id,
                user_id: conn.user_id,
                amount: receipt.amount,
                currency: receipt.currency.clone(),
                charged_at: receipt.charged_at,
                email_message_id: source_key.clone(),
                rfc_message_id: email.rfc_message_id.clone(),
                source: ChargeSource::Gmail,
                source_key,
                source_connection_id: Some(conn.id),
                provider_message_id: Some(email.provider_message_id),
                kind: if upsert.inserted {
                    ReceiptKind::NewSubscription
                } else {
                    ReceiptKind::Renewal
                },
                transaction_id: None,
                match_status: ChargeMatchStatus::Pending,
                match_started_at: now,
                match_source: None,
                created_at: now,
            };
            let (id, inserted) = self.charges.create_idempotent(&charge).await?;
            if inserted {
                new_charge_ids.push(id);
            }
        }
        self.connections
            .update_sync_cursor(conn.id, Utc::now(), batch.next_history_id)
            .await?;
        Ok(new_charge_ids)
    }

    pub async fn list(
        &self,
        user_id: Uuid,
        status: Option<SubscriptionStatus>,
    ) -> anyhow::Result<Vec<Subscription>> {
        self.subscriptions
            .list_by_user(user_id, &SubscriptionListFilter { status })
            .await
    }

    pub async fn get(&self, user_id: Uuid, id: Uuid) -> anyhow::Result<Subscription> {
        self.subscriptions
            .find_by_id(id, user_id)
            .await?
            .ok_or_else(|| SubscriptionError::SubscriptionNotFound.into())
    }

    pub async fn list_charges(
        &self,
        user_id: Uuid,
        subscription_id: Uuid,
        limit: Option<usize>,
    ) -> anyhow::Result<Vec<SubscriptionCharge>> {
        self.get(user_id, subscription_id).await?;
        let mut charges = self
            .charges
            .list_for_subscription(subscription_id, user_id)
            .await?;
        if let Some(limit) = limit {
            charges.truncate(limit);
        }
        Ok(charges)
    }

    pub async fn mark_transaction_as_subscription(
        &self,
        user_id: Uuid,
        transaction_id: Uuid,
        target: TransactionSubscriptionTarget,
    ) -> anyhow::Result<MarkTransactionSubscriptionResult> {
        let outcome = self
            .subscriptions
            .mark_transaction_as_subscription(&MarkTransactionSubscription {
                user_id,
                transaction_id,
                target,
                requested_at: Utc::now(),
            })
            .await?;
        let (subscription_id, charge_id, subscription_created, already_linked) = match outcome {
            MarkTransactionSubscriptionOutcome::Created {
                subscription_id,
                charge_id,
                subscription_created,
            } => (subscription_id, charge_id, subscription_created, false),
            MarkTransactionSubscriptionOutcome::AlreadyLinked {
                subscription_id,
                charge_id,
            } => (subscription_id, charge_id, false, true),
            MarkTransactionSubscriptionOutcome::TransactionNotFound => {
                return Err(crate::domain::error::DomainError::NotFound(format!(
                    "transaction {transaction_id}"
                ))
                .into());
            }
            MarkTransactionSubscriptionOutcome::TransactionNotExpense => {
                return Err(crate::domain::error::DomainError::InvalidInput(
                    "only expense transactions can be marked as subscriptions".into(),
                )
                .into());
            }
            MarkTransactionSubscriptionOutcome::TransactionInvalid => {
                return Err(crate::domain::error::DomainError::InvalidInput(
                    "transaction amount and currency must be valid".into(),
                )
                .into());
            }
            MarkTransactionSubscriptionOutcome::SubscriptionNotFound => {
                return Err(SubscriptionError::SubscriptionNotFound.into());
            }
            MarkTransactionSubscriptionOutcome::TransactionAlreadyLinked {
                subscription_id,
                ..
            } => {
                return Err(crate::domain::error::DomainError::Conflict(format!(
                    "transaction is already linked to subscription {subscription_id}"
                ))
                .into());
            }
        };
        let subscription = self.get(user_id, subscription_id).await?;
        let charge = self.find_charge(user_id, charge_id).await?;
        Ok(MarkTransactionSubscriptionResult {
            subscription,
            charge,
            subscription_created,
            already_linked,
        })
    }

    pub async fn find_transaction_links(
        &self,
        user_id: Uuid,
        transaction_ids: &[Uuid],
    ) -> anyhow::Result<Vec<TransactionSubscriptionLink>> {
        self.charges
            .find_transaction_links(user_id, transaction_ids)
            .await
    }

    pub async fn update_overrides(
        &self,
        user_id: Uuid,
        id: Uuid,
        product_name: Option<Option<String>>,
        category_id: Option<Option<Uuid>>,
        billing_period: Option<Option<BillingPeriod>>,
        status: Option<Option<SubscriptionStatus>>,
    ) -> anyhow::Result<Subscription> {
        self.get(user_id, id).await?;
        if let Some(Some(category_id)) = category_id {
            self.categories
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("category validation is not configured"))?
                .require_owned(category_id, user_id)
                .await?;
        }
        self.subscriptions
            .update_editable_fields(
                id,
                user_id,
                product_name,
                category_id,
                billing_period,
                status,
            )
            .await?;
        self.get(user_id, id).await
    }

    pub async fn delete_subscription(&self, user_id: Uuid, id: Uuid) -> anyhow::Result<()> {
        self.get(user_id, id).await?;
        self.subscriptions.delete(id, user_id).await
    }

    pub async fn find_charge(&self, user_id: Uuid, id: Uuid) -> anyhow::Result<SubscriptionCharge> {
        self.charges
            .find_by_id(id, user_id)
            .await?
            .ok_or_else(|| SubscriptionError::ChargeNotFound.into())
    }

    pub async fn manual_link_charge(
        &self,
        user_id: Uuid,
        charge_id: Uuid,
        transaction_id: Uuid,
    ) -> anyhow::Result<crate::domain::subscription_charge::ChargeLinkOutcome> {
        self.find_charge(user_id, charge_id).await?;
        self.charges
            .link_transaction(
                charge_id,
                user_id,
                transaction_id,
                crate::domain::subscription_charge::ChargeMatchSource::Manual,
            )
            .await
    }

    pub async fn manual_unlink_charge(&self, user_id: Uuid, charge_id: Uuid) -> anyhow::Result<()> {
        let charge = self.find_charge(user_id, charge_id).await?;
        if charge.transaction_id.is_none() {
            return Err(
                crate::domain::error::DomainError::Conflict("charge is not linked".into()).into(),
            );
        }
        if !self
            .charges
            .unlink_transaction(charge_id, user_id, true)
            .await?
        {
            return Err(SubscriptionError::ChargeNotFound.into());
        }
        Ok(())
    }

    pub async fn forecast_next_30d(
        &self,
        user_id: Uuid,
        base_currency: &str,
        fx: &dyn FxRateRepository,
    ) -> anyhow::Result<Forecast> {
        let subs = self
            .subscriptions
            .list_by_user(
                user_id,
                &SubscriptionListFilter {
                    status: Some(SubscriptionStatus::Active),
                },
            )
            .await?;
        let window_start = Utc::now();
        let window_end = window_start + chrono::Duration::days(30);
        let base_currency = base_currency.trim().to_ascii_uppercase();
        let mut by_currency: HashMap<String, Decimal> = HashMap::new();
        let mut normalized_by_currency: HashMap<String, NormalizedCurrencyTotals> = HashMap::new();
        for s in subs {
            let currency = s.currency.trim().to_ascii_uppercase();
            let yearly = match s.billing_period {
                BillingPeriod::Weekly => s.amount * Decimal::from(52),
                BillingPeriod::Monthly => s.amount * Decimal::from(12),
                BillingPeriod::Yearly => s.amount,
            };
            let monthly = yearly / Decimal::from(12);
            let normalized = normalized_by_currency.entry(currency.clone()).or_insert(
                NormalizedCurrencyTotals {
                    monthly: Decimal::ZERO,
                    yearly: Decimal::ZERO,
                },
            );
            normalized.monthly += monthly;
            normalized.yearly += yearly;

            let mut next_due = if s.overrides.billing_period.is_some() {
                s.billing_period
                    .next_after(s.last_charged_at.unwrap_or(s.started_at))
            } else {
                s.next_expected_at
                    .or_else(|| s.last_charged_at.map(|at| s.billing_period.next_after(at)))
                    .unwrap_or_else(|| s.billing_period.next_after(s.started_at))
            };
            let mut safety = 0_u16;
            while next_due < window_start && safety < 600 {
                next_due = s.billing_period.next_after(next_due);
                safety += 1;
            }
            while next_due <= window_end && safety < 600 {
                *by_currency.entry(currency.clone()).or_insert(Decimal::ZERO) += s.amount;
                next_due = s.billing_period.next_after(next_due);
                safety += 1;
            }
        }

        let quote_date = window_start.date_naive();
        let currencies = by_currency
            .keys()
            .chain(normalized_by_currency.keys())
            .cloned()
            .collect::<HashSet<_>>();
        let mut base_total = Decimal::ZERO;
        let mut monthly_equivalent_total = Decimal::ZERO;
        let mut yearly_equivalent_total = Decimal::ZERO;
        let mut fx_quotes = Vec::new();
        let mut warnings = Vec::new();
        for currency in currencies {
            let quote = fx
                .quote_as_of(quote_date, &currency, &base_currency)
                .await?;
            let Some(quote) = quote else {
                warnings.push(format!(
                    "missing FX quote {currency} → {base_currency}; totals exclude this currency"
                ));
                continue;
            };
            if let Some(actual) = by_currency.get(&currency) {
                base_total += *actual * quote.rate;
            }
            if let Some(normalized) = normalized_by_currency.get(&currency) {
                monthly_equivalent_total += normalized.monthly * quote.rate;
                yearly_equivalent_total += normalized.yearly * quote.rate;
            }
            fx_quotes.push(ForecastFxQuote {
                from_currency: quote.from_currency,
                to_currency: quote.to_currency,
                rate: quote.rate,
                requested_date: quote.requested_date,
                rate_date: quote.rate_date,
            });
        }
        fx_quotes.sort_by(|left, right| left.from_currency.cmp(&right.from_currency));
        Ok(Forecast {
            window_start,
            window_end,
            base_currency,
            base_total,
            by_currency,
            monthly_equivalent_total,
            yearly_equivalent_total,
            normalized_by_currency,
            fx_quotes,
            complete: warnings.is_empty(),
            warnings,
        })
    }

    #[cfg(test)]
    async fn upsert_subscription_from_receipt(
        &self,
        user_id: Uuid,
        receipt: &ParsedReceipt,
    ) -> anyhow::Result<Option<crate::domain::subscription::SubscriptionUpsertResult>> {
        let billing_period = receipt
            .billing_period_hint
            .unwrap_or(BillingPeriod::Monthly);
        let now = Utc::now();
        let sub = Subscription {
            id: Uuid::new_v4(),
            user_id,
            provider: receipt.provider.clone(),
            product_name: receipt.product_name.clone(),
            merchant_key: receipt.merchant_key.clone(),
            amount: receipt.amount,
            currency: receipt.currency.clone(),
            billing_period,
            status: SubscriptionStatus::Active,
            started_at: receipt.charged_at,
            last_charged_at: Some(receipt.charged_at),
            next_expected_at: Some(billing_period.next_after(receipt.charged_at)),
            category_id: None,
            overrides: Default::default(),
            created_at: now,
        };
        self.subscriptions
            .upsert_receipt_if_not_tombstoned(&sub)
            .await
    }
}

fn is_invalid_credentials(error: &anyhow::Error) -> bool {
    if matches!(
        error.downcast_ref::<GmailProviderError>(),
        Some(GmailProviderError::InvalidCredentials)
    ) {
        return true;
    }
    if let Some(error) = error.downcast_ref::<reqwest::Error>() {
        return error.status() == Some(reqwest::StatusCode::UNAUTHORIZED);
    }
    error
        .to_string()
        .eq_ignore_ascii_case("Gmail refresh token is missing")
}

#[cfg(test)]
mod test_support {
    use super::*;

    pub struct FakeFetcher {
        pub emails: Vec<RawEmail>,
    }

    #[async_trait::async_trait]
    impl EmailFetcher for FakeFetcher {
        async fn fetch_new(&self, _conn: &EmailConnection) -> anyhow::Result<EmailFetchBatch> {
            Ok(EmailFetchBatch {
                emails: self.emails.clone(),
                failures: vec![],
                ignored_message_ids: vec![],
                next_history_id: Some("cursor-1".to_string()),
                history_was_reset: false,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::FakeFetcher;
    use super::*;
    use crate::domain::subscription::SubscriptionRepository;
    use crate::domain::subscription_charge::SubscriptionChargeRepository;
    use crate::infrastructure::email_connection_repository::PgEmailConnectionRepository;
    use crate::infrastructure::subscription_charge_repository::PgSubscriptionChargeRepository;
    use crate::infrastructure::subscription_repository::PgSubscriptionRepository;
    use crate::infrastructure::test_db;

    fn make_svc(pool: sqlx::PgPool) -> SubscriptionService {
        let conns: Arc<dyn EmailConnectionRepository> =
            Arc::new(PgEmailConnectionRepository::new(pool.clone()));
        let subs: Arc<dyn SubscriptionRepository> =
            Arc::new(PgSubscriptionRepository::new(pool.clone()));
        let charges: Arc<dyn SubscriptionChargeRepository> =
            Arc::new(PgSubscriptionChargeRepository::new(pool.clone()));
        SubscriptionService::new(
            conns,
            subs,
            charges,
            Arc::new(FakeFetcher { emails: vec![] }),
            Arc::new(ParserRegistry::default_set()),
        )
    }

    #[tokio::test]
    async fn connect_gmail_persists_connected_status() {
        let pool = test_db::fresh_pool().await;
        let repo: Arc<dyn EmailConnectionRepository> =
            Arc::new(PgEmailConnectionRepository::new(pool.clone()));
        let svc = make_svc(pool);
        let user_id = Uuid::new_v4();
        let conn = svc
            .connect_gmail(
                user_id,
                ConnectGmailParams {
                    email_address: "x@y.com".into(),
                    access_token: "a".into(),
                    refresh_token: "r".into(),
                    expires_at: Utc::now() + chrono::Duration::hours(1),
                },
            )
            .await
            .unwrap();
        assert_eq!(conn.status, EmailConnectionStatus::Connected);
        let found = repo.find_by_id(conn.id, user_id).await.unwrap().unwrap();
        assert_eq!(found.email_address, "x@y.com");
    }

    #[tokio::test]
    async fn delete_returns_error_when_missing() {
        let pool = test_db::fresh_pool().await;
        let svc = make_svc(pool);
        let err = svc
            .delete_connection(Uuid::new_v4(), Uuid::new_v4())
            .await
            .unwrap_err();
        assert!(err.downcast_ref::<SubscriptionError>().is_some());
    }
}

#[cfg(test)]
mod sync_tests {
    use super::test_support::FakeFetcher;
    use super::*;
    use crate::domain::email_connection::EmailConnectionRepository;
    use crate::domain::subscription::SubscriptionRepository;
    use crate::domain::subscription_charge::SubscriptionChargeRepository;
    use crate::infrastructure::email_connection_repository::PgEmailConnectionRepository;
    use crate::infrastructure::subscription_charge_repository::PgSubscriptionChargeRepository;
    use crate::infrastructure::subscription_repository::PgSubscriptionRepository;
    use crate::infrastructure::test_db;

    fn netflix_email(msg_id: &str) -> RawEmail {
        RawEmail {
            provider_message_id: msg_id.to_string(),
            rfc_message_id: Some(format!("<{msg_id}>")),
            from: "Netflix <info@account.netflix.com>".into(),
            subject: "Your Netflix payment".into(),
            authentication_results: vec![],
            received_at: Utc::now(),
            body_text: Some("Plan: Netflix Premium\nTotal: $15.99 USD\nDate: May 18, 2026".into()),
            body_html: None,
        }
    }

    #[tokio::test]
    async fn sync_creates_subscription_and_charge_then_is_idempotent() {
        let pool = test_db::fresh_pool().await;
        let user_id = Uuid::new_v4();
        let conns: Arc<dyn EmailConnectionRepository> =
            Arc::new(PgEmailConnectionRepository::new(pool.clone()));
        let subs: Arc<dyn SubscriptionRepository> =
            Arc::new(PgSubscriptionRepository::new(pool.clone()));
        let charges: Arc<dyn SubscriptionChargeRepository> =
            Arc::new(PgSubscriptionChargeRepository::new(pool.clone()));
        let svc = SubscriptionService::new(
            conns.clone(),
            subs,
            charges.clone(),
            Arc::new(FakeFetcher {
                emails: vec![netflix_email("<m1>")],
            }),
            Arc::new(ParserRegistry::default_set()),
        );

        let conn = svc
            .connect_gmail(
                user_id,
                ConnectGmailParams {
                    email_address: "x@y.com".into(),
                    access_token: "a".into(),
                    refresh_token: "r".into(),
                    expires_at: Utc::now() + chrono::Duration::hours(1),
                },
            )
            .await
            .unwrap();

        let ids1 = svc.sync_connection(conn.id).await.unwrap();
        assert_eq!(ids1.len(), 1);

        // Second sync with same email → 0 new ids (idempotent on email_message_id).
        let ids2 = svc.sync_connection(conn.id).await.unwrap();
        assert!(ids2.is_empty());

        let pending = charges.list_pending_for_user(user_id).await.unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].amount.to_string(), "15.99");
    }
}

#[cfg(test)]
mod reliable_sync_tests {
    use super::*;
    use crate::domain::email_connection::EmailConnectionRepository;
    use crate::domain::email_sync::EmailSyncRepository;
    use crate::domain::subscription::SubscriptionRepository;
    use crate::domain::subscription_charge::SubscriptionChargeRepository;
    use crate::infrastructure::email::oauth::{GmailOAuthClient, GmailProfile, GmailTokenSet};
    use crate::infrastructure::email_connection_repository::PgEmailConnectionRepository;
    use crate::infrastructure::email_sync_repository::PgEmailSyncRepository;
    use crate::infrastructure::subscription_charge_repository::PgSubscriptionChargeRepository;
    use crate::infrastructure::subscription_repository::PgSubscriptionRepository;
    use crate::infrastructure::test_db;

    #[derive(Clone, Copy)]
    enum FetchMode {
        InvalidCredentials,
        TransientFailure,
        IgnoredMessage,
    }

    struct ControlledFetcher {
        mode: FetchMode,
    }

    #[async_trait::async_trait]
    impl EmailFetcher for ControlledFetcher {
        async fn fetch_new(&self, _conn: &EmailConnection) -> anyhow::Result<EmailFetchBatch> {
            match self.mode {
                FetchMode::InvalidCredentials => Err(GmailProviderError::InvalidCredentials.into()),
                FetchMode::TransientFailure => Err(GmailProviderError::Transient.into()),
                FetchMode::IgnoredMessage => Ok(EmailFetchBatch {
                    emails: vec![],
                    failures: vec![],
                    ignored_message_ids: vec!["spoofed-message".to_string()],
                    next_history_id: Some("cursor-after-ignored".to_string()),
                    history_was_reset: false,
                }),
            }
        }
    }

    struct UnusedOAuthClient;

    #[async_trait::async_trait]
    impl GmailOAuthClient for UnusedOAuthClient {
        fn authorization_url(&self, _state: &str, _pkce_challenge: &str) -> anyhow::Result<String> {
            anyhow::bail!("OAuth client is not used by this test")
        }

        async fn exchange_code(
            &self,
            _code: &str,
            _pkce_verifier: &str,
        ) -> anyhow::Result<GmailTokenSet> {
            anyhow::bail!("OAuth client is not used by this test")
        }

        async fn profile(&self, _access_token: &str) -> anyhow::Result<GmailProfile> {
            anyhow::bail!("OAuth client is not used by this test")
        }

        async fn refresh(&self, _refresh_token: &str) -> anyhow::Result<GmailTokenSet> {
            anyhow::bail!("OAuth client is not used by this test")
        }

        async fn revoke(&self, _token: &str) -> anyhow::Result<()> {
            anyhow::bail!("OAuth client is not used by this test")
        }
    }

    fn reliable_service(
        pool: sqlx::PgPool,
        mode: FetchMode,
    ) -> (SubscriptionService, Arc<dyn EmailConnectionRepository>) {
        let connections: Arc<dyn EmailConnectionRepository> =
            Arc::new(PgEmailConnectionRepository::new(pool.clone()));
        let subscriptions: Arc<dyn SubscriptionRepository> =
            Arc::new(PgSubscriptionRepository::new(pool.clone()));
        let charges: Arc<dyn SubscriptionChargeRepository> =
            Arc::new(PgSubscriptionChargeRepository::new(pool.clone()));
        let sync: Arc<dyn EmailSyncRepository> = Arc::new(PgEmailSyncRepository::new(pool.clone()));
        let service = SubscriptionService::new(
            Arc::clone(&connections),
            subscriptions,
            charges,
            Arc::new(ControlledFetcher { mode }),
            Arc::new(ParserRegistry::default_set()),
        )
        .with_reliable_sync(sync, Arc::new(UnusedOAuthClient));
        (service, connections)
    }

    async fn connected_mailbox(
        service: &SubscriptionService,
        connections: &Arc<dyn EmailConnectionRepository>,
        user_id: Uuid,
    ) -> EmailConnection {
        let connection = service
            .connect_gmail(
                user_id,
                ConnectGmailParams {
                    email_address: format!("{user_id}@example.com"),
                    access_token: "access".into(),
                    refresh_token: "refresh".into(),
                    expires_at: Utc::now() + chrono::Duration::hours(1),
                },
            )
            .await
            .unwrap();
        connections
            .update_sync_cursor(
                connection.id,
                Utc::now() - chrono::Duration::hours(1),
                Some("cursor-before".to_string()),
            )
            .await
            .unwrap();
        connection
    }

    #[tokio::test]
    async fn invalid_credentials_require_reconnect_without_advancing_cursor() {
        let pool = test_db::fresh_pool().await;
        let (service, connections) = reliable_service(pool.clone(), FetchMode::InvalidCredentials);
        let user_id = Uuid::new_v4();
        let connection = connected_mailbox(&service, &connections, user_id).await;

        let before = Utc::now().timestamp();
        let error = service.sync_connection(connection.id).await.unwrap_err();
        assert!(matches!(
            error.downcast_ref::<GmailProviderError>(),
            Some(GmailProviderError::InvalidCredentials)
        ));
        let state: (
            String,
            Option<String>,
            i64,
            i32,
            Option<String>,
            Option<Uuid>,
        ) = sqlx::query_as(
            "SELECT status,last_history_id,next_sync_at,sync_attempts,\
                        sync_last_error_kind,sync_lease_owner \
                 FROM email_connections WHERE id=$1",
        )
        .bind(connection.id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(state.0, "reconnect_required");
        assert_eq!(state.1.as_deref(), Some("cursor-before"));
        assert!((before + 300..=Utc::now().timestamp() + 301).contains(&state.2));
        assert_eq!(state.3, 1);
        assert_eq!(state.4.as_deref(), Some("invalid_credentials"));
        assert_eq!(state.5, None);
    }

    #[tokio::test]
    async fn transient_provider_failure_keeps_connection_retryable_and_cursor_safe() {
        let pool = test_db::fresh_pool().await;
        let (service, connections) = reliable_service(pool.clone(), FetchMode::TransientFailure);
        let user_id = Uuid::new_v4();
        let connection = connected_mailbox(&service, &connections, user_id).await;

        let before = Utc::now().timestamp();
        let error = service.sync_connection(connection.id).await.unwrap_err();
        assert!(matches!(
            error.downcast_ref::<GmailProviderError>(),
            Some(GmailProviderError::Transient)
        ));
        let state: (String, Option<String>, i64, i32, Option<String>) = sqlx::query_as(
            "SELECT status,last_history_id,next_sync_at,sync_attempts,sync_last_error_kind \
             FROM email_connections WHERE id=$1",
        )
        .bind(connection.id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(state.0, "connected");
        assert_eq!(state.1.as_deref(), Some("cursor-before"));
        assert!((before + 300..=Utc::now().timestamp() + 301).contains(&state.2));
        assert_eq!(state.3, 1);
        assert_eq!(state.4.as_deref(), Some("transient_sync_failure"));
    }

    #[tokio::test]
    async fn provider_filtered_message_is_recorded_ignored_before_cursor_advances() {
        let pool = test_db::fresh_pool().await;
        let (service, connections) = reliable_service(pool.clone(), FetchMode::IgnoredMessage);
        let user_id = Uuid::new_v4();
        let connection = connected_mailbox(&service, &connections, user_id).await;

        assert!(
            service
                .sync_connection(connection.id)
                .await
                .unwrap()
                .is_empty()
        );
        let cursor: Option<String> =
            sqlx::query_scalar("SELECT last_history_id FROM email_connections WHERE id=$1")
                .bind(connection.id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(cursor.as_deref(), Some("cursor-after-ignored"));
        let outcome: (String, Option<String>, Option<i64>) = sqlx::query_as(
            "SELECT outcome,error_kind,next_retry_at FROM email_message_ingestions \
             WHERE connection_id=$1 AND provider_message_id='spoofed-message'",
        )
        .bind(connection.id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(outcome.0, "ignored");
        assert_eq!(
            outcome.1.as_deref(),
            Some("untrusted_sender_authentication")
        );
        assert_eq!(outcome.2, None);
    }

    #[tokio::test]
    async fn only_manual_resync_requeues_upgrade_sensitive_ignored_messages() {
        let pool = test_db::fresh_pool().await;
        let (service, connections) = reliable_service(pool.clone(), FetchMode::IgnoredMessage);
        let user_id = Uuid::new_v4();
        let connection = connected_mailbox(&service, &connections, user_id).await;
        let sync = PgEmailSyncRepository::new(pool.clone());
        let recorded_at = Utc::now() - chrono::Duration::days(1);

        sync.record_ignored(
            connection.id,
            user_id,
            "parser-upgrade-candidate",
            None,
            recorded_at,
            "not_recurring",
        )
        .await
        .unwrap();
        sync.record_ignored(
            connection.id,
            user_id,
            "permanent-ignore",
            None,
            recorded_at,
            "subscription_tombstoned",
        )
        .await
        .unwrap();

        // Ordinary scheduler-driven syncs must not revisit durable ignored
        // rows, even when they are candidates for a future parser upgrade.
        service.sync_connection(connection.id).await.unwrap();
        let scheduled_state: (String, Option<i64>) = sqlx::query_as(
            "SELECT outcome,next_retry_at FROM email_message_ingestions \
             WHERE connection_id=$1 AND provider_message_id='parser-upgrade-candidate'",
        )
        .bind(connection.id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(scheduled_state, ("ignored".to_string(), None));

        // Claiming an explicit manual resync makes upgrade-sensitive rows due
        // immediately, while permanent outcomes remain ignored.
        let _job = service
            .claim_connection_for_user(connection.id, user_id)
            .await
            .unwrap();
        let manual_state: (String, i32, Option<String>, Option<i64>) = sqlx::query_as(
            "SELECT outcome,attempts,error_kind,next_retry_at \
             FROM email_message_ingestions \
             WHERE connection_id=$1 AND provider_message_id='parser-upgrade-candidate'",
        )
        .bind(connection.id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(manual_state.0, "failed");
        assert_eq!(manual_state.1, 0);
        assert_eq!(manual_state.2.as_deref(), Some("manual_requeue"));
        assert!(
            manual_state
                .3
                .is_some_and(|due_at| due_at >= recorded_at.timestamp())
        );

        let permanent_state: (String, Option<String>, Option<i64>) = sqlx::query_as(
            "SELECT outcome,error_kind,next_retry_at FROM email_message_ingestions \
             WHERE connection_id=$1 AND provider_message_id='permanent-ignore'",
        )
        .bind(connection.id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(
            permanent_state,
            (
                "ignored".to_string(),
                Some("subscription_tombstoned".to_string()),
                None,
            )
        );
    }
}

#[cfg(test)]
mod forecast_tests {
    use std::sync::Arc;

    use chrono::Utc;
    use rust_decimal_macros::dec;
    use uuid::Uuid;

    use super::test_support::FakeFetcher;
    use super::*;
    use crate::domain::email_connection::EmailConnectionRepository;
    use crate::domain::fx_rate::FxRateRepository;
    use crate::domain::subscription::{
        BillingPeriod, Subscription, SubscriptionProvider, SubscriptionRepository,
        SubscriptionStatus,
    };
    use crate::domain::subscription_charge::SubscriptionChargeRepository;
    use crate::infrastructure::email_connection_repository::PgEmailConnectionRepository;
    use crate::infrastructure::fx_rate_repository::PgFxRateRepository;
    use crate::infrastructure::subscription_charge_repository::PgSubscriptionChargeRepository;
    use crate::infrastructure::subscription_repository::PgSubscriptionRepository;
    use crate::infrastructure::test_db;

    #[tokio::test]
    async fn forecast_sums_active_subs_normalized_to_monthly() {
        let pool = test_db::fresh_pool().await;
        let user_id = Uuid::new_v4();
        let now = Utc::now();
        let conns: Arc<dyn EmailConnectionRepository> =
            Arc::new(PgEmailConnectionRepository::new(pool.clone()));
        let subs: Arc<dyn SubscriptionRepository> =
            Arc::new(PgSubscriptionRepository::new(pool.clone()));
        let charges: Arc<dyn SubscriptionChargeRepository> =
            Arc::new(PgSubscriptionChargeRepository::new(pool.clone()));
        let fx: Arc<dyn FxRateRepository> = Arc::new(PgFxRateRepository::new(pool.clone()));
        let svc = SubscriptionService::new(
            conns,
            subs.clone(),
            charges,
            Arc::new(FakeFetcher { emails: vec![] }),
            Arc::new(ParserRegistry::default_set()),
        );

        subs.upsert_by_merchant_key(&Subscription {
            id: Uuid::new_v4(),
            user_id,
            provider: SubscriptionProvider::Netflix,
            product_name: "Netflix".into(),
            merchant_key: "netflix.com:premium".into(),
            amount: dec!(15.99),
            currency: "USD".into(),
            billing_period: BillingPeriod::Monthly,
            status: SubscriptionStatus::Active,
            started_at: now,
            last_charged_at: None,
            next_expected_at: Some(now + chrono::Duration::days(1)),
            category_id: None,
            overrides: Default::default(),
            created_at: now,
        })
        .await
        .unwrap();

        subs.upsert_by_merchant_key(&Subscription {
            id: Uuid::new_v4(),
            user_id,
            provider: SubscriptionProvider::AppleAppStore,
            product_name: "iCloud+".into(),
            merchant_key: "apps.apple.com:icloud_50gb".into(),
            amount: dec!(12.00),
            currency: "USD".into(),
            billing_period: BillingPeriod::Yearly,
            status: SubscriptionStatus::Active,
            started_at: now,
            last_charged_at: None,
            next_expected_at: Some(now + chrono::Duration::days(45)),
            category_id: None,
            overrides: Default::default(),
            created_at: now,
        })
        .await
        .unwrap();

        subs.upsert_by_merchant_key(&Subscription {
            id: Uuid::new_v4(),
            user_id,
            provider: SubscriptionProvider::Other,
            product_name: "Weekly storage".into(),
            merchant_key: "example.com:weekly-storage".into(),
            amount: dec!(2.00),
            currency: "USD".into(),
            billing_period: BillingPeriod::Weekly,
            status: SubscriptionStatus::Active,
            started_at: now,
            last_charged_at: None,
            next_expected_at: Some(now + chrono::Duration::days(1)),
            category_id: None,
            overrides: Default::default(),
            created_at: now,
        })
        .await
        .unwrap();

        let f = svc.forecast_next_30d(user_id, "USD", &*fx).await.unwrap();
        // The monthly charge occurs once and the weekly charge occurs five
        // times. The yearly charge is outside the window but still contributes
        // to normalized cost totals.
        assert_eq!(f.base_total, dec!(25.99));
        assert_eq!(f.by_currency["USD"], dec!(25.99));
        assert!(f.normalized_by_currency["USD"].monthly > dec!(25.65));
        assert!(f.normalized_by_currency["USD"].monthly < dec!(25.67));
        assert_eq!(f.normalized_by_currency["USD"].yearly, dec!(307.88));

        let incomplete = svc.forecast_next_30d(user_id, "UAH", &*fx).await.unwrap();
        assert_eq!(incomplete.by_currency["USD"], dec!(25.99));
        assert_eq!(incomplete.base_total, Decimal::ZERO);
        assert!(!incomplete.complete);
        assert_eq!(incomplete.warnings.len(), 1);
    }
}
