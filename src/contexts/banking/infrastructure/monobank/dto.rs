use serde::Deserialize;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ClientInfoDto {
    pub accounts: Vec<AccountDto>,
    #[serde(default)]
    pub jars: Vec<JarDto>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct AccountDto {
    pub id: String,
    pub currency_code: u16,
    pub balance: i64,
    pub credit_limit: i64,
    #[serde(default)]
    pub masked_pan: Vec<String>,
    #[serde(rename = "type")]
    pub product_type: String,
    #[serde(default)]
    pub iban: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct JarDto {
    pub id: String,
    pub title: String,
    pub currency_code: u16,
    pub balance: i64,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct StatementItemDto {
    pub id: String,
    pub time: i64,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub mcc: Option<i32>,
    #[serde(default)]
    pub hold: bool,
    pub amount: i64,
    pub operation_amount: i64,
    pub currency_code: u16,
    pub balance: i64,
}

#[derive(Deserialize)]
pub(super) struct WebhookDto {
    #[serde(rename = "type")]
    pub event_type: String,
    pub data: WebhookDataDto,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct WebhookDataDto {
    pub account: String,
    pub statement_item: StatementItemDto,
}
