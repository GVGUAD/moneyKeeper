use std::collections::BTreeMap;

use rust_decimal::Decimal;

use crate::{
    contexts::banking::{
        application::{NormalizedResource, NormalizedSnapshot, ProviderFailureClass},
        domain::{FundingModel, ResourceKind},
    },
    shared_kernel::{CurrencyCode, Money},
};

use super::dto::{ClientInfoDto, StatementItemDto, WebhookDto};

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
            let external_resource_id = remote_id(account.id, "invalid provider account id")?;
            let (currency, scale) = currencies.get(&account.currency_code).cloned().ok_or(
                crate::contexts::banking::domain::BankingError::InvalidValue(
                    "unknown numeric currency",
                ),
            )?;
            let kind = match account.product_type.as_str() {
                "black" | "white" | "platinum" | "iron" | "eAid" | "yellow" => ResourceKind::Card,
                "fop" => ResourceKind::CurrentAccount,
                _ => ResourceKind::Unsupported,
            };
            let funding_model = if kind == ResourceKind::Unsupported {
                FundingModel::Unknown
            } else if kind == ResourceKind::Card && account.credit_limit > 0 {
                FundingModel::RevolvingCredit
            } else {
                FundingModel::OwnFunds
            };
            resources.push(NormalizedResource {
                external_resource_id,
                kind,
                funding_model,
                currency: currency.clone(),
                masked_label: bounded_label(
                    account
                        .masked_pan
                        .first()
                        .cloned()
                        .filter(|value| !value.is_empty())
                        .or_else(|| (!account.iban.is_empty()).then_some(account.iban))
                        .unwrap_or_else(|| "unavailable".to_owned()),
                ),
                provider_balance: minor_money(account.balance, currency.clone(), scale)?,
                credit_limit: (account.credit_limit > 0)
                    .then(|| minor_money(account.credit_limit, currency, scale))
                    .transpose()?,
            });
        }
        for jar in dto.jars {
            let external_resource_id = remote_id(jar.id, "invalid provider jar id")?;
            let (currency, scale) = currencies.get(&jar.currency_code).cloned().ok_or(
                crate::contexts::banking::domain::BankingError::InvalidValue(
                    "unknown numeric currency",
                ),
            )?;
            resources.push(NormalizedResource {
                external_resource_id,
                kind: ResourceKind::Jar,
                funding_model: FundingModel::OwnFunds,
                currency: currency.clone(),
                masked_label: bounded_label(jar.title),
                provider_balance: minor_money(jar.balance, currency, scale)?,
                credit_limit: None,
            });
        }
        Ok(NormalizedSnapshot { resources })
    }

    pub(crate) fn normalize_statement(
        body: &str,
        resource_currency: &CurrencyCode,
        currencies: &BTreeMap<u16, (CurrencyCode, u8)>,
    ) -> Result<
        Vec<crate::contexts::banking::application::NormalizedProviderEvent>,
        crate::contexts::banking::domain::BankingError,
    > {
        let items: Vec<StatementItemDto> = serde_json::from_str(body).map_err(|_| {
            crate::contexts::banking::domain::BankingError::InvalidValue(
                "invalid provider statement response",
            )
        })?;
        normalize_items(items, resource_currency, currencies)
    }

    pub(crate) fn normalize_webhook(
        body: &[u8],
        resource_currency: &CurrencyCode,
        currencies: &BTreeMap<u16, (CurrencyCode, u8)>,
    ) -> Result<
        (
            String,
            crate::contexts::banking::application::NormalizedProviderEvent,
        ),
        crate::contexts::banking::domain::BankingError,
    > {
        let webhook: WebhookDto = serde_json::from_slice(body).map_err(|_| {
            crate::contexts::banking::domain::BankingError::InvalidValue("invalid webhook payload")
        })?;
        if webhook.event_type != "StatementItem" {
            return Err(
                crate::contexts::banking::domain::BankingError::InvalidValue(
                    "unsupported webhook payload",
                ),
            );
        }
        let mut events = normalize_items(
            vec![webhook.data.statement_item],
            resource_currency,
            currencies,
        )?;
        Ok((
            remote_id(webhook.data.account, "invalid webhook account id")?,
            events.remove(0),
        ))
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

impl crate::contexts::banking::application::ProviderNormalizer for MonobankAdapter {
    fn client_info(
        &self,
        body: &str,
        currencies: &BTreeMap<u16, (CurrencyCode, u8)>,
    ) -> Result<
        crate::contexts::banking::application::NormalizedSnapshot,
        crate::contexts::banking::domain::BankingError,
    > {
        Self::normalize_client_info(body, currencies)
    }

    fn statement(
        &self,
        body: &str,
        resource_currency: &CurrencyCode,
        currencies: &BTreeMap<u16, (CurrencyCode, u8)>,
    ) -> Result<
        Vec<crate::contexts::banking::application::NormalizedProviderEvent>,
        crate::contexts::banking::domain::BankingError,
    > {
        Self::normalize_statement(body, resource_currency, currencies)
    }

    fn webhook(
        &self,
        body: &[u8],
        resource_currency: &CurrencyCode,
        currencies: &BTreeMap<u16, (CurrencyCode, u8)>,
    ) -> Result<
        (
            String,
            crate::contexts::banking::application::NormalizedProviderEvent,
        ),
        crate::contexts::banking::domain::BankingError,
    > {
        Self::normalize_webhook(body, resource_currency, currencies)
    }
}

fn normalize_items(
    items: Vec<StatementItemDto>,
    resource_currency: &CurrencyCode,
    currencies: &BTreeMap<u16, (CurrencyCode, u8)>,
) -> Result<
    Vec<crate::contexts::banking::application::NormalizedProviderEvent>,
    crate::contexts::banking::domain::BankingError,
> {
    let resource_scale = currencies
        .values()
        .find_map(|(code, scale)| (code == resource_currency).then_some(*scale))
        .ok_or(
            crate::contexts::banking::domain::BankingError::InvalidValue(
                "resource currency is unavailable",
            ),
        )?;
    items
        .into_iter()
        .map(|item| {
            if item.amount == 0 {
                return Err(
                    crate::contexts::banking::domain::BankingError::InvalidValue(
                        "invalid provider statement item",
                    ),
                );
            }
            let (original_currency, original_scale) =
                currencies.get(&item.currency_code).cloned().ok_or(
                    crate::contexts::banking::domain::BankingError::InvalidValue(
                        "unknown statement currency",
                    ),
                )?;
            let effective_at = chrono::DateTime::<chrono::Utc>::from_timestamp(item.time, 0)
                .ok_or(
                    crate::contexts::banking::domain::BankingError::InvalidValue(
                        "invalid provider statement timestamp",
                    ),
                )?;
            let description: String = item.description.chars().take(500).collect();
            Ok(
                crate::contexts::banking::application::NormalizedProviderEvent {
                    external_event_id: remote_id(item.id, "invalid provider statement id")?,
                    state: if item.hold {
                        crate::contexts::banking::domain::ProviderTransactionState::Pending
                    } else {
                        crate::contexts::banking::domain::ProviderTransactionState::Settled
                    },
                    operation_money: minor_money(
                        item.amount,
                        resource_currency.clone(),
                        resource_scale,
                    )?,
                    original_money: Some(minor_money(
                        item.operation_amount,
                        original_currency,
                        original_scale,
                    )?),
                    description,
                    merchant_mcc: item.mcc.filter(|mcc| (0..=9999).contains(mcc)),
                    effective_at,
                    running_balance: Some(minor_money(
                        item.balance,
                        resource_currency.clone(),
                        resource_scale,
                    )?),
                },
            )
        })
        .collect()
}

fn remote_id(
    value: String,
    error: &'static str,
) -> Result<String, crate::contexts::banking::domain::BankingError> {
    if value.trim() != value || value.is_empty() || value.chars().count() > 200 {
        return Err(crate::contexts::banking::domain::BankingError::InvalidValue(error));
    }
    Ok(value)
}

fn bounded_label(value: String) -> String {
    let value: String = value.trim().chars().take(200).collect();
    if value.is_empty() {
        "unavailable".to_owned()
    } else {
        value
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statement_uses_account_currency_and_retains_original_evidence() {
        let currencies = BTreeMap::from([
            (980, (CurrencyCode::new("UAH").unwrap(), 2)),
            (840, (CurrencyCode::new("USD").unwrap(), 2)),
        ]);
        let body = r#"[{"id":"tx-1","time":1787572800,"description":"Shop","mcc":5411,"hold":true,"amount":-4100,"operationAmount":-100,"currencyCode":840,"balance":95900}]"#;
        let events = MonobankAdapter::normalize_statement(
            body,
            &CurrencyCode::new("UAH").unwrap(),
            &currencies,
        )
        .unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].state,
            crate::contexts::banking::domain::ProviderTransactionState::Pending
        );
        assert_eq!(events[0].operation_money.amount(), Decimal::new(-4100, 2));
        assert_eq!(events[0].operation_money.currency().as_str(), "UAH");
        let original = events[0].original_money.as_ref().unwrap();
        assert_eq!(original.amount(), Decimal::new(-100, 2));
        assert_eq!(original.currency().as_str(), "USD");
    }

    #[test]
    fn settled_statement_revision_is_not_pending() {
        let currencies = BTreeMap::from([(980, (CurrencyCode::new("UAH").unwrap(), 2))]);
        let body = r#"[{"id":"tx-1","time":1787572800,"description":"Shop","hold":false,"amount":-4100,"operationAmount":-4100,"currencyCode":980,"balance":95900}]"#;
        let events = MonobankAdapter::normalize_statement(
            body,
            &CurrencyCode::new("UAH").unwrap(),
            &currencies,
        )
        .unwrap();
        assert_eq!(
            events[0].state,
            crate::contexts::banking::domain::ProviderTransactionState::Settled
        );
    }
}
