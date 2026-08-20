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
