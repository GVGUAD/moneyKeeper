use chrono::{NaiveDate, TimeZone, Utc};
use regex::Regex;
use rust_decimal::Decimal;
use scraper::{Html, Selector};
use std::str::FromStr;

use crate::domain::email::RawEmail;
use crate::domain::receipt_parser::{ParsedReceipt, ReceiptParser};
use crate::domain::subscription::{BillingPeriod, SubscriptionProvider};
use crate::domain::subscription_charge::ReceiptKind;

pub struct AppleParser;

impl AppleParser {
    pub fn new() -> Self {
        Self
    }
}

impl Default for AppleParser {
    fn default() -> Self {
        Self::new()
    }
}

impl ReceiptParser for AppleParser {
    fn matches_sender(&self, from: &str) -> bool {
        from.to_ascii_lowercase()
            .contains("no_reply@email.apple.com")
    }

    fn parse(&self, email: &RawEmail) -> anyhow::Result<Option<ParsedReceipt>> {
        let html = match email.body_html.as_deref() {
            Some(h) => h,
            None => return Ok(None),
        };
        let doc = Html::parse_document(html);
        let p_sel = Selector::parse("p").unwrap();
        let mut text = String::new();
        for el in doc.select(&p_sel) {
            text.push_str(&el.text().collect::<String>());
            text.push('\n');
        }

        let name_re = Regex::new(r"(?i)App Name:\s*(.+)").unwrap();
        let sub_re = Regex::new(r"(?i)Subscription:\s*(\w+)").unwrap();
        let amount_re = Regex::new(r"(?i)Amount Billed:\s*\$?([0-9]+(?:\.[0-9]{2})?)").unwrap();
        let date_re = Regex::new(r"(?i)Receipt Date:\s*([A-Za-z]+ [0-9]{1,2}, [0-9]{4})").unwrap();

        let product_name = name_re
            .captures(&text)
            .and_then(|c| c.get(1).map(|m| m.as_str().trim().to_string()))
            .ok_or_else(|| anyhow::anyhow!("app name not found in Apple receipt"))?;

        let period_str = sub_re
            .captures(&text)
            .and_then(|c| c.get(1).map(|m| m.as_str().to_ascii_lowercase()))
            .unwrap_or_default();
        let billing_period_hint = match period_str.as_str() {
            "monthly" => Some(BillingPeriod::Monthly),
            "yearly" | "annual" => Some(BillingPeriod::Yearly),
            "weekly" => Some(BillingPeriod::Weekly),
            _ => None,
        };

        let amount_str = amount_re
            .captures(&text)
            .and_then(|c| c.get(1).map(|m| m.as_str().to_string()))
            .ok_or_else(|| anyhow::anyhow!("amount not found in Apple receipt"))?;
        let amount = Decimal::from_str(&amount_str)?;
        let currency = "USD".to_string();

        let date_str = date_re
            .captures(&text)
            .and_then(|c| c.get(1).map(|m| m.as_str().to_string()))
            .ok_or_else(|| anyhow::anyhow!("date not found in Apple receipt"))?;
        let naive = NaiveDate::parse_from_str(&date_str, "%B %d, %Y")?;
        let charged_at = Utc.from_utc_datetime(&naive.and_hms_opt(0, 0, 0).unwrap());

        let merchant_key = format!(
            "apps.apple.com:{}",
            product_name.to_ascii_lowercase().replace(' ', "_")
        );

        Ok(Some(ParsedReceipt {
            provider: SubscriptionProvider::AppleAppStore,
            product_name,
            merchant_key,
            amount,
            currency,
            charged_at,
            billing_period_hint,
            kind: ReceiptKind::Renewal,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_email() -> RawEmail {
        let html = std::fs::read_to_string("tests/fixtures/receipts/apple/renewal.html").unwrap();
        RawEmail {
            message_id: "<apple-1>".to_string(),
            from: "Apple <no_reply@email.apple.com>".to_string(),
            subject: "Your receipt from Apple".to_string(),
            received_at: Utc::now(),
            body_text: None,
            body_html: Some(html),
        }
    }

    #[test]
    fn matches_sender() {
        let p = AppleParser::new();
        assert!(p.matches_sender("Apple <no_reply@email.apple.com>"));
        assert!(!p.matches_sender("info@account.netflix.com"));
    }

    #[test]
    fn parses_renewal_fixture() {
        let p = AppleParser::new();
        let r = p.parse(&fixture_email()).unwrap().unwrap();
        assert_eq!(r.provider, SubscriptionProvider::AppleAppStore);
        assert!(r.product_name.contains("iCloud"));
        assert_eq!(r.amount.to_string(), "0.99");
        assert_eq!(r.currency, "USD");
        assert_eq!(r.billing_period_hint, Some(BillingPeriod::Monthly));
    }

    #[test]
    fn returns_none_when_no_html() {
        let p = AppleParser::new();
        let email = RawEmail {
            message_id: "<apple-2>".to_string(),
            from: "Apple <no_reply@email.apple.com>".to_string(),
            subject: "Your receipt from Apple".to_string(),
            received_at: Utc::now(),
            body_text: Some("some text".to_string()),
            body_html: None,
        };
        assert!(p.parse(&email).unwrap().is_none());
    }
}
