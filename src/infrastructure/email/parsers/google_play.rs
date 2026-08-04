use chrono::{NaiveDate, TimeZone, Utc};
use regex::Regex;
use rust_decimal::Decimal;
use scraper::{Html, Selector};
use std::str::FromStr;

use crate::domain::email::RawEmail;
use crate::domain::receipt_parser::{ParsedReceipt, ReceiptParser};
use crate::domain::subscription::{BillingPeriod, SubscriptionProvider};
use crate::domain::subscription_charge::ReceiptKind;

pub struct GooglePlayParser;

impl GooglePlayParser {
    pub fn new() -> Self {
        Self
    }
}

impl Default for GooglePlayParser {
    fn default() -> Self {
        Self::new()
    }
}

impl ReceiptParser for GooglePlayParser {
    fn matches_sender(&self, from: &str) -> bool {
        from.to_ascii_lowercase()
            .contains("googleplay-noreply@google.com")
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

        let item_re = Regex::new(r"(?i)Item:\s*(.+?)\s*\(([^)]+)\)").unwrap();
        let price_re = Regex::new(r"(?i)Price:\s*(\$|[A-Z]{3} )?([0-9]+(?:\.[0-9]{2})?)").unwrap();
        let date_re =
            Regex::new(r"(?i)Order completed:\s*([A-Za-z]+ [0-9]{1,2}, [0-9]{4})").unwrap();

        let item_caps = item_re
            .captures(&text)
            .ok_or_else(|| anyhow::anyhow!("item not found in Google Play receipt"))?;
        let product_name = item_caps.get(1).unwrap().as_str().trim().to_string();
        let period_str = item_caps.get(2).unwrap().as_str().to_ascii_lowercase();
        let billing_period_hint = match period_str.as_str() {
            "monthly" => Some(BillingPeriod::Monthly),
            "yearly" | "annual" => Some(BillingPeriod::Yearly),
            "weekly" => Some(BillingPeriod::Weekly),
            _ => None,
        };

        let price_caps = price_re
            .captures(&text)
            .ok_or_else(|| anyhow::anyhow!("price not found in Google Play receipt"))?;
        let prefix = price_caps.get(1).map(|m| m.as_str().trim().to_string());
        let amount = Decimal::from_str(price_caps.get(2).unwrap().as_str())?;
        let currency = match prefix.as_deref() {
            Some("$") => "USD".to_string(),
            Some(other) if !other.is_empty() => other.to_string(),
            _ => "USD".to_string(),
        };

        let date_str = date_re
            .captures(&text)
            .and_then(|c| c.get(1).map(|m| m.as_str().to_string()))
            .ok_or_else(|| anyhow::anyhow!("date not found in Google Play receipt"))?;
        let naive = NaiveDate::parse_from_str(&date_str, "%B %d, %Y")?;
        let charged_at = Utc.from_utc_datetime(&naive.and_hms_opt(0, 0, 0).unwrap());

        let merchant_key = format!(
            "play.google.com:{}",
            product_name.to_ascii_lowercase().replace(' ', "_")
        );

        Ok(Some(ParsedReceipt {
            provider: SubscriptionProvider::GooglePlay,
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
        let html =
            std::fs::read_to_string("tests/fixtures/receipts/google_play/renewal.html").unwrap();
        RawEmail {
            message_id: "<gp-1>".to_string(),
            from: "Google Play <googleplay-noreply@google.com>".to_string(),
            subject: "Your Google Play Order Receipt".to_string(),
            received_at: Utc::now(),
            body_text: None,
            body_html: Some(html),
        }
    }

    #[test]
    fn matches_sender() {
        let p = GooglePlayParser::new();
        assert!(p.matches_sender("Google Play <googleplay-noreply@google.com>"));
        assert!(!p.matches_sender("info@account.netflix.com"));
    }

    #[test]
    fn parses_renewal_fixture() {
        let p = GooglePlayParser::new();
        let r = p.parse(&fixture_email()).unwrap().unwrap();
        assert_eq!(r.provider, SubscriptionProvider::GooglePlay);
        assert!(r.product_name.contains("Notion"));
        assert_eq!(r.amount.to_string(), "9.99");
        assert_eq!(r.currency, "USD");
        assert_eq!(r.billing_period_hint, Some(BillingPeriod::Monthly));
    }
}
