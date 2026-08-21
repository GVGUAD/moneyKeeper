//! Characterized receipt parsers retained behind the Mail adapter boundary.
pub(crate) mod apple;
pub(crate) mod google_play;
pub(crate) mod netflix;
use crate::domain::receipt_parser::ReceiptParser;
pub(crate) struct ParsedBy {
    pub parser_name: &'static str,
    pub parser_version: i32,
    pub receipt: crate::domain::receipt_parser::ParsedReceipt,
}
pub(crate) struct ParserRegistry {
    parsers: Vec<(&'static str, i32, Box<dyn ReceiptParser>)>,
}
impl ParserRegistry {
    pub(crate) fn default_set() -> Self {
        Self {
            parsers: vec![
                ("netflix", 1, Box::new(netflix::NetflixParser::new())),
                (
                    "google_play",
                    1,
                    Box::new(google_play::GooglePlayParser::new()),
                ),
                ("apple", 1, Box::new(apple::AppleParser::new())),
            ],
        }
    }
    pub(crate) fn find(&self, from: &str) -> Option<&dyn ReceiptParser> {
        self.parsers
            .iter()
            .find(|(_, _, parser)| parser.matches_sender(from))
            .map(|(_, _, parser)| parser.as_ref())
    }
    pub(crate) fn parse(
        &self,
        email: &crate::domain::email::RawEmail,
    ) -> anyhow::Result<Option<ParsedBy>> {
        let Some((name, version, parser)) = self
            .parsers
            .iter()
            .find(|(_, _, parser)| parser.matches_sender(&email.from))
        else {
            return Ok(None);
        };
        parser.parse(email).map(|receipt| {
            receipt.map(|receipt| ParsedBy {
                parser_name: name,
                parser_version: *version,
                receipt,
            })
        })
    }
}
