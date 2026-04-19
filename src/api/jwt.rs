use jsonwebtoken::jwk::JwkSet;
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header};
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct Claims {
    pub sub: String,           // Supabase user UUID
    pub email: Option<String>, // absent for some OAuth providers
    pub role: String,          // "authenticated"
    pub aud: String,           // "authenticated"
    pub exp: i64,
    pub iat: i64,
}

pub fn verify_token(token: &str, jwks: &JwkSet) -> anyhow::Result<Claims> {
    let header = decode_header(token)?;
    let kid = header
        .kid
        .ok_or_else(|| anyhow::anyhow!("missing kid in JWT header"))?;
    let jwk = jwks
        .find(&kid)
        .ok_or_else(|| anyhow::anyhow!("no matching JWK for kid: {kid}"))?;
    let decoding_key = DecodingKey::from_jwk(jwk)?;
    let mut validation = Validation::new(Algorithm::ES256);
    validation.set_audience(&["authenticated"]);
    let data = decode::<Claims>(token, &decoding_key, &validation)?;
    Ok(data.claims)
}
