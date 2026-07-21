use chrono::{DateTime, FixedOffset, NaiveDate, NaiveTime, TimeZone, Utc};
use regex::Regex;
use rust_decimal::Decimal;
use std::str::FromStr;
use std::sync::OnceLock;

use crate::domain::email::RawEmail;
use crate::domain::receipt_parser::{ParsedReceipt, ReceiptParser};
use crate::domain::subscription::{BillingPeriod, SubscriptionProvider};

use super::{html_visible_text, is_explicit_non_recurring, normalized_mailbox};

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
        normalized_mailbox(from) == "googleplay-noreply@google.com"
    }

    fn parse(&self, email: &RawEmail) -> anyhow::Result<Option<ParsedReceipt>> {
        let text = match email.body_html.as_deref() {
            Some(html) => html_visible_text(html),
            None => match email.body_text.as_deref() {
                Some(text) => text.to_string(),
                None => return Ok(None),
            },
        };

        if is_explicit_non_recurring(&email.subject, &text)
            || google_play_non_recurring_regex().is_match(&text)
        {
            return Ok(None);
        }

        let line_item = match parse_legacy_line_item(&text)? {
            Some(line_item) => Some(line_item),
            None => parse_order_table_line_item(&text)?,
        };
        let Some((product_name, amount, currency, billing_period_hint)) = line_item else {
            return Ok(None);
        };
        if amount <= Decimal::ZERO {
            return Ok(None);
        }

        let charged_at = parse_charge_date(&text)?;

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
        }))
    }
}

fn item_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(r"(?i)Item:\s*(.+?)\s*\((weekly|monthly|yearly|annual)\)")
            .expect("valid Google Play item regex")
    })
}

fn price_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(
            r"(?i)Price:\s*(?:(\$|€|£|₴|грн\.?|[A-Z]{3})\s*)?((?:[0-9][0-9.,\x{00a0}\x{202f} ]*[0-9])|[0-9])(?:\s*(\$|€|£|₴|грн\.?|\b[A-Z]{3}\b))?",
        )
            .expect("valid Google Play price regex")
    })
}

fn recurring_price_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(
            r"(?i)(?:(\$|€|£|₴|грн\.?|[A-Z]{3})\s*)?((?:[0-9][0-9.,\x{00a0}\x{202f} ]*[0-9])|[0-9])\s*(?:(\$|€|£|₴|грн\.?|\b[A-Z]{3}\b))?\s*/\s*(week|month|year)\b",
        )
        .expect("valid Google Play recurring price regex")
    })
}

fn item_price_header_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(r"(?im)(?:^|\n)\s*Item\s+Price\s+")
            .expect("valid Google Play order table header regex")
    })
}

fn date_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(
            r"(?i)Order (?:completed|date):\s*([A-Za-z]+ [0-9]{1,2}, [0-9]{4})(?:\s+([0-9]{1,2}:[0-9]{2}(?::[0-9]{2})?\s*[AP]M)(?:\s+GMT\s*([+-]\s*[0-9]{1,2}(?::?[0-9]{2})?))?)?",
        )
            .expect("valid Google Play date regex")
    })
}

fn google_play_non_recurring_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(
            r"(?im)^\s*(?:Your (?:subscription|order).*(?:has been|was)\s+(?:cancelled|canceled|refunded)|Refund issued:)\b",
        )
        .expect("valid Google Play non-recurring regex")
    })
}

type ParsedLineItem = (String, Decimal, String, Option<BillingPeriod>);

fn parse_legacy_line_item(text: &str) -> anyhow::Result<Option<ParsedLineItem>> {
    let Some(item_caps) = item_regex().captures(text) else {
        return Ok(None);
    };
    let Some(period) = billing_period(item_caps.get(2).unwrap().as_str()) else {
        return Ok(None);
    };
    let Some(price_caps) = price_regex().captures(text) else {
        return Ok(None);
    };
    let currency_value = price_caps
        .get(1)
        .or_else(|| price_caps.get(3))
        .map(|value| value.as_str());
    let Some(currency) = normalize_currency(currency_value) else {
        return Ok(None);
    };
    let amount = parse_amount(price_caps.get(2).unwrap().as_str())?;

    Ok(Some((
        normalize_product_name(item_caps.get(1).unwrap().as_str()),
        amount,
        currency,
        Some(period),
    )))
}

fn parse_order_table_line_item(text: &str) -> anyhow::Result<Option<ParsedLineItem>> {
    let Some(price_caps) = recurring_price_regex().captures(text) else {
        return Ok(None);
    };
    let price_match = price_caps.get(0).unwrap();
    let Some(header) = item_price_header_regex()
        .find_iter(&text[..price_match.start()])
        .last()
    else {
        return Ok(None);
    };
    let product_name = normalize_product_name(&text[header.end()..price_match.start()]);
    if product_name.is_empty() {
        return Ok(None);
    }

    let currency_value = price_caps
        .get(1)
        .or_else(|| price_caps.get(3))
        .map(|value| value.as_str());
    let Some(currency) = normalize_currency(currency_value) else {
        return Ok(None);
    };
    let Some(period) = billing_period(price_caps.get(4).unwrap().as_str()) else {
        return Ok(None);
    };

    let amount = parse_amount(price_caps.get(2).unwrap().as_str())?;
    Ok(Some((product_name, amount, currency, Some(period))))
}

fn billing_period(value: &str) -> Option<BillingPeriod> {
    match value.trim().to_ascii_lowercase().as_str() {
        "week" | "weekly" => Some(BillingPeriod::Weekly),
        "month" | "monthly" => Some(BillingPeriod::Monthly),
        "year" | "yearly" | "annual" => Some(BillingPeriod::Yearly),
        _ => None,
    }
}

fn normalize_product_name(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn parse_amount(value: &str) -> anyhow::Result<Decimal> {
    let mut normalized = value
        .replace([' ', '\u{00a0}', '\u{202f}'], "")
        .trim()
        .to_string();
    if let (Some(comma), Some(dot)) = (normalized.rfind(','), normalized.rfind('.')) {
        if comma > dot {
            normalized = normalized.replace('.', "").replace(',', ".");
        } else {
            normalized = normalized.replace(',', "");
        }
    } else if normalized.contains(',') {
        normalized = normalized.replace(',', ".");
    }
    Ok(Decimal::from_str(&normalized)?)
}

fn parse_charge_date(text: &str) -> anyhow::Result<DateTime<Utc>> {
    let caps = date_regex()
        .captures(text)
        .ok_or_else(|| anyhow::anyhow!("date not found in Google Play receipt"))?;
    let date = NaiveDate::parse_from_str(caps.get(1).unwrap().as_str(), "%B %d, %Y")?;
    let Some(time_match) = caps.get(2) else {
        return Ok(Utc.from_utc_datetime(&date.and_hms_opt(0, 0, 0).unwrap()));
    };

    let time_value = time_match
        .as_str()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_uppercase();
    let time_format = if time_value.matches(':').count() == 2 {
        "%I:%M:%S %p"
    } else {
        "%I:%M %p"
    };
    let time = NaiveTime::parse_from_str(&time_value, time_format)?;
    let offset_seconds = parse_gmt_offset(caps.get(3).map(|value| value.as_str()))?;
    let offset = FixedOffset::east_opt(offset_seconds)
        .ok_or_else(|| anyhow::anyhow!("invalid GMT offset in Google Play receipt"))?;
    let local = date.and_time(time);
    let charged_at = offset
        .from_local_datetime(&local)
        .single()
        .ok_or_else(|| anyhow::anyhow!("invalid local date in Google Play receipt"))?;
    Ok(charged_at.with_timezone(&Utc))
}

fn parse_gmt_offset(value: Option<&str>) -> anyhow::Result<i32> {
    let Some(value) = value else {
        return Ok(0);
    };
    let compact = value.replace(' ', "");
    let sign = match compact.as_bytes().first() {
        Some(b'+') => 1,
        Some(b'-') => -1,
        _ => return Err(anyhow::anyhow!("invalid GMT offset in Google Play receipt")),
    };
    let digits = &compact[1..];
    let (hours, minutes) = if let Some((hours, minutes)) = digits.split_once(':') {
        (hours, minutes)
    } else if digits.len() > 2 {
        digits.split_at(digits.len() - 2)
    } else {
        (digits, "0")
    };
    let hours: i32 = hours.parse()?;
    let minutes: i32 = minutes.parse()?;
    if hours > 23 || minutes > 59 {
        return Err(anyhow::anyhow!("invalid GMT offset in Google Play receipt"));
    }
    Ok(sign * (hours * 60 * 60 + minutes * 60))
}

fn normalize_currency(value: Option<&str>) -> Option<String> {
    let value = value?.trim().trim_end_matches('.');
    match value.to_ascii_uppercase().as_str() {
        "$" => Some("USD".to_string()),
        "€" => Some("EUR".to_string()),
        "£" => Some("GBP".to_string()),
        "₴" => Some("UAH".to_string()),
        code if code.len() == 3 && code.bytes().all(|b| b.is_ascii_alphabetic()) => {
            Some(code.to_string())
        }
        _ if value.eq_ignore_ascii_case("грн") => Some("UAH".to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_email() -> RawEmail {
        let html =
            std::fs::read_to_string("tests/fixtures/receipts/google_play/renewal.html").unwrap();
        RawEmail {
            provider_message_id: "gmail-gp-1".to_string(),
            rfc_message_id: Some("<gp-1>".to_string()),
            from: "Google Play <googleplay-noreply@google.com>".to_string(),
            subject: "Your Google Play Order Receipt".to_string(),
            authentication_results: vec![],
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

    #[test]
    fn ignores_one_time_order_and_spoofed_sender() {
        let p = GooglePlayParser::new();
        let mut email = fixture_email();
        email.subject = "Your one-time Google Play order".into();
        assert!(p.parse(&email).unwrap().is_none());
        assert!(!p.matches_sender("googleplay-noreply@google.com.evil.test"));
    }

    #[test]
    fn recurring_footer_does_not_turn_receipt_into_cancellation() {
        assert!(
            GooglePlayParser::new()
                .parse(&fixture_email())
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn parses_div_based_renewal_fixture() {
        let mut email = fixture_email();
        email.body_html = Some(
            std::fs::read_to_string("tests/fixtures/receipts/google_play/div_renewal.html")
                .unwrap(),
        );
        let receipt = GooglePlayParser::new().parse(&email).unwrap().unwrap();
        assert_eq!(receipt.product_name, "Google One 100 GB");
        assert_eq!(receipt.amount.to_string(), "1.99");
        assert_eq!(receipt.currency, "USD");
    }

    #[test]
    fn ignores_explicit_one_time_fixture() {
        let mut email = fixture_email();
        email.subject = "Your one-time Google Play order".to_string();
        email.body_html = Some(
            std::fs::read_to_string("tests/fixtures/receipts/google_play/one_time.html").unwrap(),
        );
        assert!(GooglePlayParser::new().parse(&email).unwrap().is_none());
    }

    #[test]
    fn parses_realistic_uah_renewal_fixture() {
        let mut email = fixture_email();
        email.subject = "Your Google Play Order Receipt from Jun 26, 2026".to_string();
        email.body_html = Some(
            std::fs::read_to_string("tests/fixtures/receipts/google_play/real_uah_renewal.html")
                .unwrap(),
        );

        let receipt = GooglePlayParser::new().parse(&email).unwrap().unwrap();
        assert_eq!(
            receipt.product_name,
            "Monthly recipe subscription full access (Example Recipes)"
        );
        assert_eq!(receipt.amount.to_string(), "189.00");
        assert_eq!(receipt.currency, "UAH");
        assert_eq!(receipt.billing_period_hint, Some(BillingPeriod::Monthly));
        assert_eq!(
            receipt.charged_at,
            chrono::DateTime::parse_from_rfc3339("2026-06-26T11:58:48Z")
                .unwrap()
                .with_timezone(&Utc)
        );
    }

    #[test]
    fn realistic_refund_and_cancellation_are_not_receipts() {
        let mut email = fixture_email();
        email.body_html = Some(
            std::fs::read_to_string("tests/fixtures/receipts/google_play/real_uah_renewal.html")
                .unwrap(),
        );

        email.subject = "Your Google Play order was refunded".to_string();
        assert!(GooglePlayParser::new().parse(&email).unwrap().is_none());

        email.subject = "Your Google Play Order Receipt".to_string();
        email.body_html = Some(
            email
                .body_html
                .unwrap()
                .replace("has renewed", "has been canceled"),
        );
        assert!(GooglePlayParser::new().parse(&email).unwrap().is_none());
    }
}
