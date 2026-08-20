use std::collections::BTreeMap;

use rust_decimal::Decimal;

use crate::{
    contexts::banking::{
        application::ProviderFailureClass,
        domain::{FundingModel, ResourceKind},
    },
    shared_kernel::{CurrencyCode, Money},
};

use super::dto::ClientInfoDto;

#[derive(Clone)]
pub struct NormalizedResource {
    pub external_resource_id: String,
    pub kind: ResourceKind,
    pub funding_model: FundingModel,
    pub currency: CurrencyCode,
    pub masked_label: String,
    pub provider_balance: Money,
    pub credit_limit: Option<Money>,
}

impl std::fmt::Debug for NormalizedResource {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("NormalizedResource")
            .field("external_resource_id", &"[REDACTED]")
            .field("kind", &self.kind)
            .field("funding_model", &self.funding_model)
            .field("currency", &self.currency)
            .field("masked_label", &"[REDACTED]")
            .field("provider_balance", &self.provider_balance)
            .field("credit_limit", &self.credit_limit)
            .finish()
    }
}

#[derive(Clone, Debug)]
pub struct NormalizedSnapshot {
    pub resources: Vec<NormalizedResource>,
}

pub struct MonobankAdapter;

impl MonobankAdapter {
    pub fn normalize_client_info(
        body: &str,
        currencies: &BTreeMap<u16, (CurrencyCode, u8)>,
    ) -> Result<NormalizedSnapshot, crate::contexts::banking::domain::BankingError> {
        let dto: ClientInfoDto = serde_json::from_str(body).map_err(|_| {
            crate::contexts::banking::domain::BankingError::InvalidValue(
                "invalid provider response",
            )
        })?;
        let mut resources = Vec::with_capacity(dto.accounts.len() + dto.jars.len());
        for account in dto.accounts {
            let (currency, scale) = currencies.get(&account.currency_code).cloned().ok_or(
                crate::contexts::banking::domain::BankingError::InvalidValue(
                    "unknown numeric currency",
                ),
            )?;
            let recognized = matches!(
                account.product_type.as_str(),
                "black" | "white" | "platinum" | "iron" | "eAid" | "yellow"
            );
            let kind = if recognized {
                ResourceKind::Card
            } else {
                ResourceKind::Unsupported
            };
            let funding_model = if !recognized {
                FundingModel::Unknown
            } else if account.credit_limit > 0 {
                FundingModel::RevolvingCredit
            } else {
                FundingModel::OwnFunds
            };
            resources.push(NormalizedResource {
                external_resource_id: account.id,
                kind,
                funding_model,
                currency: currency.clone(),
                masked_label: account
                    .masked_pan
                    .first()
                    .cloned()
                    .filter(|value| !value.is_empty())
                    .or_else(|| (!account.iban.is_empty()).then_some(account.iban))
                    .unwrap_or_else(|| "unavailable".to_owned()),
                provider_balance: minor_money(account.balance, currency.clone(), scale)?,
                credit_limit: (account.credit_limit > 0)
                    .then(|| minor_money(account.credit_limit, currency, scale))
                    .transpose()?,
            });
        }
        for jar in dto.jars {
            let (currency, scale) = currencies.get(&jar.currency_code).cloned().ok_or(
                crate::contexts::banking::domain::BankingError::InvalidValue(
                    "unknown numeric currency",
                ),
            )?;
            resources.push(NormalizedResource {
                external_resource_id: jar.id,
                kind: ResourceKind::Jar,
                funding_model: FundingModel::OwnFunds,
                currency: currency.clone(),
                masked_label: jar.title,
                provider_balance: minor_money(jar.balance, currency, scale)?,
                credit_limit: None,
            });
        }
        Ok(NormalizedSnapshot { resources })
    }

    pub const fn classify_status(status: u16) -> ProviderFailureClass {
        match status {
            429 => ProviderFailureClass::RateLimited,
            401 | 403 => ProviderFailureClass::NeedsReauth,
            500..=599 => ProviderFailureClass::Transient,
            _ => ProviderFailureClass::Terminal,
        }
    }
}

fn minor_money(
    value: i64,
    currency: CurrencyCode,
    scale: u8,
) -> Result<Money, crate::contexts::banking::domain::BankingError> {
    Money::new(
        Decimal::new(value, u32::from(scale)),
        currency,
        u32::from(scale),
    )
    .map_err(|_| {
        crate::contexts::banking::domain::BankingError::InvalidValue("provider amount is invalid")
    })
}
