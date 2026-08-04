pub mod apple;
pub mod google_play;
pub mod netflix;

use crate::domain::receipt_parser::ReceiptParser;

pub struct ParserRegistry {
    parsers: Vec<Box<dyn ReceiptParser>>,
}

impl ParserRegistry {
    pub fn default_set() -> Self {
        Self {
            parsers: vec![
                Box::new(netflix::NetflixParser::new()),
                Box::new(google_play::GooglePlayParser::new()),
                Box::new(apple::AppleParser::new()),
            ],
        }
    }

    pub fn find(&self, from: &str) -> Option<&dyn ReceiptParser> {
        self.parsers
            .iter()
            .find(|p| p.matches_sender(from))
            .map(|b| b.as_ref())
    }
}

#[cfg(test)]
mod registry_tests {
    use super::*;

    #[test]
    fn finds_netflix() {
        let reg = ParserRegistry::default_set();
        assert!(reg.find("info@account.netflix.com").is_some());
    }

    #[test]
    fn finds_google_play() {
        let reg = ParserRegistry::default_set();
        assert!(reg.find("googleplay-noreply@google.com").is_some());
    }

    #[test]
    fn finds_none_for_unknown() {
        let reg = ParserRegistry::default_set();
        assert!(reg.find("noreply@hulu.com").is_none());
    }
}
