use std::sync::Arc;

use hmac::{Hmac, Mac};
use http::HeaderMap;
use rand::RngCore;
use sha2::Sha256;
use subtle::ConstantTimeEq;
use zeroize::Zeroizing;

use crate::config::Environment;
use crate::store::{ApiClient, PgStore, StoreError};

type HmacSha256 = Hmac<Sha256>;

#[derive(Clone)]
#[allow(missing_debug_implementations)]
pub struct ApiKeyAuthenticator {
    store: PgStore,
    environment: Environment,
    pepper: Arc<Zeroizing<Vec<u8>>>,
}

#[allow(missing_debug_implementations)]
pub struct GeneratedApiKey {
    pub prefix: String,
    raw: Zeroizing<String>,
    pub digest: [u8; 32],
}

#[derive(Clone, Debug)]
pub struct AuthenticatedClient {
    pub client: ApiClient,
    pub key_prefix: String,
}

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("API authentication failed")]
    Invalid,
    #[error("authentication store unavailable")]
    Store(#[source] StoreError),
    #[error("API key authenticator is misconfigured")]
    Configuration,
}

impl ApiKeyAuthenticator {
    pub fn new(
        store: PgStore,
        environment: Environment,
        pepper: impl AsRef<[u8]>,
    ) -> Result<Self, AuthError> {
        if pepper.as_ref().len() < 32 {
            return Err(AuthError::Configuration);
        }
        Ok(Self {
            store,
            environment,
            pepper: Arc::new(Zeroizing::new(pepper.as_ref().to_vec())),
        })
    }

    pub async fn authenticate(
        &self,
        headers: &HeaderMap,
    ) -> Result<AuthenticatedClient, AuthError> {
        let raw = extract_presented_key(headers)?;
        let presented = ParsedApiKey::parse(raw, self.environment)?;
        let calculated = digest_api_key(self.pepper.as_slice(), raw)?;
        let candidate = self
            .store
            .lookup_api_key(presented.prefix)
            .await
            .map_err(AuthError::Store)?;

        // Perform a constant-time comparison even when no prefix was found so
        // the HMAC path does not reveal key-prefix existence.
        let mut expected = [0_u8; 32];
        if let Some(candidate) = &candidate
            && candidate.digest.len() == expected.len()
        {
            expected.copy_from_slice(&candidate.digest);
        }
        let digest_matches = bool::from(calculated.ct_eq(&expected));
        let candidate = candidate
            .filter(|candidate| candidate.client.environment == environment_name(self.environment));
        if !digest_matches {
            return Err(AuthError::Invalid);
        }
        candidate
            .map(|candidate| AuthenticatedClient {
                client: candidate.client,
                key_prefix: presented.prefix.to_owned(),
            })
            .ok_or(AuthError::Invalid)
    }
}

impl GeneratedApiKey {
    pub fn generate(environment: Environment, pepper: &[u8]) -> Result<Self, AuthError> {
        let mut public_id = [0_u8; 12];
        let mut secret = Zeroizing::new([0_u8; 32]);
        rand::rngs::OsRng.fill_bytes(&mut public_id);
        rand::rngs::OsRng.fill_bytes(secret.as_mut());
        let prefix = format!(
            "x402_{}_{}",
            environment.api_key_label(),
            hex::encode(public_id)
        );
        let raw = Zeroizing::new(format!("{prefix}.{}", hex::encode(secret.as_slice())));
        let digest = digest_api_key(pepper, raw.as_bytes())?;
        Ok(Self {
            prefix,
            raw,
            digest,
        })
    }

    pub fn into_parts(self) -> (String, [u8; 32], Zeroizing<String>) {
        (self.prefix, self.digest, self.raw)
    }
}

pub fn digest_api_key(pepper: &[u8], api_key: impl AsRef<[u8]>) -> Result<[u8; 32], AuthError> {
    if pepper.len() < 32 {
        return Err(AuthError::Configuration);
    }
    let mut mac = HmacSha256::new_from_slice(pepper).map_err(|_| AuthError::Configuration)?;
    mac.update(api_key.as_ref());
    Ok(mac.finalize().into_bytes().into())
}

fn extract_presented_key(headers: &HeaderMap) -> Result<&str, AuthError> {
    let x_api_key = single_header(headers, "x-api-key")?;
    let bearer = single_header(headers, "authorization")?
        .map(parse_bearer)
        .transpose()?;
    match (x_api_key, bearer) {
        (Some(left), Some(right)) if left == right => Ok(left),
        (Some(_), Some(_)) | (None, None) => Err(AuthError::Invalid),
        (Some(key), None) | (None, Some(key)) => Ok(key),
    }
}

fn single_header<'a>(
    headers: &'a HeaderMap,
    name: &'static str,
) -> Result<Option<&'a str>, AuthError> {
    let mut values = headers.get_all(name).iter();
    let first = values.next();
    if values.next().is_some() {
        return Err(AuthError::Invalid);
    }
    first
        .map(|value| value.to_str().map_err(|_| AuthError::Invalid))
        .transpose()
}

fn parse_bearer(value: &str) -> Result<&str, AuthError> {
    let mut parts = value.split_ascii_whitespace();
    let scheme = parts.next().ok_or(AuthError::Invalid)?;
    let key = parts.next().ok_or(AuthError::Invalid)?;
    if !scheme.eq_ignore_ascii_case("bearer") || parts.next().is_some() {
        return Err(AuthError::Invalid);
    }
    Ok(key)
}

struct ParsedApiKey<'a> {
    prefix: &'a str,
}

impl<'a> ParsedApiKey<'a> {
    fn parse(raw: &'a str, environment: Environment) -> Result<Self, AuthError> {
        let (prefix, secret) = raw.split_once('.').ok_or(AuthError::Invalid)?;
        if secret.contains('.') || secret.len() != 64 || !secret.bytes().all(is_lower_hex) {
            return Err(AuthError::Invalid);
        }
        let expected = format!("x402_{}_", environment.api_key_label());
        let public_id = prefix.strip_prefix(&expected).ok_or(AuthError::Invalid)?;
        if public_id.len() != 24 || !public_id.bytes().all(is_lower_hex) {
            return Err(AuthError::Invalid);
        }
        Ok(Self { prefix })
    }
}

const fn is_lower_hex(byte: u8) -> bool {
    byte.is_ascii_digit() || matches!(byte, b'a'..=b'f')
}

const fn environment_name(environment: Environment) -> &'static str {
    match environment {
        Environment::Mainnet => "mainnet",
        Environment::Testnet => "testnet",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::HeaderValue;

    fn test_key() -> &'static str {
        "x402_test_00112233445566778899aabb.\
         00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff"
    }

    #[test]
    fn parses_key_from_either_header() {
        let mut x_header = HeaderMap::new();
        x_header.insert("x-api-key", HeaderValue::from_static(test_key()));
        assert_eq!(extract_presented_key(&x_header).ok(), Some(test_key()));

        let mut bearer = HeaderMap::new();
        let value = format!("Bearer {}", test_key());
        let value = HeaderValue::from_str(&value).unwrap_or_else(|_| std::process::abort());
        bearer.insert("authorization", value);
        assert_eq!(extract_presented_key(&bearer).ok(), Some(test_key()));
    }

    #[test]
    fn rejects_conflicting_or_malformed_headers() {
        let mut headers = HeaderMap::new();
        headers.insert("x-api-key", HeaderValue::from_static(test_key()));
        headers.insert(
            "authorization",
            HeaderValue::from_static(
                "Bearer x402_test_ffffffffffffffffffffffff.\
                 ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
            ),
        );
        assert!(extract_presented_key(&headers).is_err());
        assert!(ParsedApiKey::parse("x402_test_bad.short", Environment::Testnet).is_err());
        assert!(ParsedApiKey::parse(test_key(), Environment::Mainnet).is_err());
        assert!(
            ParsedApiKey::parse(
                "x402_test_00112233445566778899AABB.\
             00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff",
                Environment::Testnet,
            )
            .is_err()
        );
    }

    #[test]
    fn generated_keys_have_strict_shape_and_stable_digest() {
        let pepper = [42_u8; 32];
        let key = GeneratedApiKey::generate(Environment::Testnet, &pepper)
            .unwrap_or_else(|_| std::process::abort());
        let expected_digest = key.digest;
        let (_prefix, _digest, raw) = key.into_parts();
        assert!(ParsedApiKey::parse(raw.as_str(), Environment::Testnet).is_ok());
        assert_eq!(
            digest_api_key(&pepper, raw.as_str()).ok(),
            Some(expected_digest)
        );
    }
}
