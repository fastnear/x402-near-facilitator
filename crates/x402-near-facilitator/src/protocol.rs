use std::{cmp::Ordering, fmt};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use x402_types::proto;

use crate::config::PaymentIdentifierConfig;

pub const PAYMENT_IDENTIFIER_EXTENSION: &str = "payment-identifier";
const REQUEST_FINGERPRINT_DOMAIN: &[u8] = b"x402-near/request/v1\0";

#[derive(Clone)]
pub struct ParsedRequest {
    pub raw: proto::VerifyRequest,
    pub value: Value,
    pub meta: RequestMeta,
}

impl fmt::Debug for ParsedRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ParsedRequest")
            .field("raw", &"<redacted>")
            .field("value", &"<redacted>")
            .field("meta", &self.meta)
            .finish()
    }
}

#[derive(Clone, Eq, PartialEq)]
pub struct RequestMeta {
    pub x402_version: u8,
    pub scheme: String,
    pub network: String,
    pub asset: String,
    pub amount: String,
    pub pay_to: String,
    pub signed_delegate_action: String,
    pub payment_identifier: Option<String>,
}

impl fmt::Debug for RequestMeta {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RequestMeta")
            .field("x402_version", &self.x402_version)
            .field("scheme", &self.scheme)
            .field("network", &self.network)
            .field("asset", &self.asset)
            .field("amount", &self.amount)
            .field("pay_to", &"<redacted>")
            .field("signed_delegate_action", &"<redacted>")
            .field("payment_identifier", &"<redacted>")
            .finish()
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifyResponse {
    pub is_valid: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub invalid_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub invalid_message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payer: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extensions: Option<Map<String, Value>>,
}

impl VerifyResponse {
    pub fn valid(payer: String) -> Self {
        Self {
            is_valid: true,
            invalid_reason: None,
            invalid_message: None,
            payer: Some(payer),
            extensions: None,
        }
    }

    pub fn invalid(
        reason: impl Into<String>,
        message: Option<String>,
        payer: Option<String>,
    ) -> Self {
        Self {
            is_valid: false,
            invalid_reason: Some(reason.into()),
            invalid_message: message,
            payer,
            extensions: None,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SettleResponse {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payer: Option<String>,
    pub transaction: String,
    pub network: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extensions: Option<Map<String, Value>>,
}

impl SettleResponse {
    pub fn success(payer: String, transaction: String, network: String) -> Self {
        Self {
            success: true,
            error_reason: None,
            error_message: None,
            payer: Some(payer),
            transaction,
            network,
            extensions: None,
        }
    }

    pub fn failure(
        reason: impl Into<String>,
        message: Option<String>,
        payer: Option<String>,
        transaction: String,
        network: String,
    ) -> Self {
        Self {
            success: false,
            error_reason: Some(reason.into()),
            error_message: message,
            payer,
            transaction,
            network,
            extensions: None,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SupportedKind {
    pub x402_version: u8,
    pub scheme: String,
    pub network: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extra: Option<Value>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SupportedResponse {
    pub kinds: Vec<SupportedKind>,
    pub extensions: Vec<String>,
    pub signers: std::collections::HashMap<String, Vec<String>>,
}

#[derive(Debug, thiserror::Error)]
pub enum RequestError {
    #[error("request body must be a JSON object")]
    NotObject,
    #[error("missing or invalid field {0}")]
    Field(&'static str),
    #[error("malformed payment-identifier extension: {0}")]
    PaymentIdentifier(String),
    #[error("failed to preserve request JSON: {0}")]
    Json(#[from] serde_json::Error),
}

pub fn parse_request(
    body: &[u8],
    identifier_policy: &PaymentIdentifierConfig,
) -> Result<ParsedRequest, RequestError> {
    let value: Value = serde_json::from_slice(body)?;
    let object = value.as_object().ok_or(RequestError::NotObject)?;
    ensure_allowed_keys(
        object,
        &["x402Version", "paymentPayload", "paymentRequirements"],
        "request",
    )?;
    let x402_version = object
        .get("x402Version")
        .and_then(Value::as_u64)
        .and_then(|value| u8::try_from(value).ok())
        .ok_or(RequestError::Field("x402Version"))?;
    let payload = object
        .get("paymentPayload")
        .and_then(Value::as_object)
        .ok_or(RequestError::Field("paymentPayload"))?;
    ensure_allowed_keys(
        payload,
        &[
            "x402Version",
            "resource",
            "accepted",
            "payload",
            "extensions",
        ],
        "paymentPayload",
    )?;
    required_u8(payload, "x402Version", "paymentPayload.x402Version")?;
    validate_resource(payload.get("resource"))?;
    let requirements = object
        .get("paymentRequirements")
        .and_then(Value::as_object)
        .ok_or(RequestError::Field("paymentRequirements"))?;
    validate_requirements_shape(requirements, "paymentRequirements")?;
    let accepted = payload
        .get("accepted")
        .and_then(Value::as_object)
        .ok_or(RequestError::Field("paymentPayload.accepted"))?;
    validate_requirements_shape(accepted, "paymentPayload.accepted")?;
    let mechanism_payload = payload
        .get("payload")
        .and_then(Value::as_object)
        .ok_or(RequestError::Field("paymentPayload.payload"))?;
    ensure_allowed_keys(
        mechanism_payload,
        &["signedDelegateAction"],
        "paymentPayload.payload",
    )?;
    if let Some(extensions) = payload.get("extensions")
        && !extensions.is_object()
    {
        return Err(RequestError::Field("paymentPayload.extensions"));
    }

    let scheme = required_string(requirements, "scheme", "paymentRequirements.scheme")?;
    let network = required_string(requirements, "network", "paymentRequirements.network")?;
    let asset = required_string(requirements, "asset", "paymentRequirements.asset")?;
    let amount = required_decimal_string(requirements, "amount", "paymentRequirements.amount")?;
    let pay_to = required_string(requirements, "payTo", "paymentRequirements.payTo")?;
    let signed_delegate_action = required_string(
        mechanism_payload,
        "signedDelegateAction",
        "paymentPayload.payload.signedDelegateAction",
    )?;

    // These fields must at least have the expected scalar shape at the HTTP
    // boundary. Exact equality and chain semantics remain the mechanism's job.
    required_string(accepted, "scheme", "paymentPayload.accepted.scheme")?;
    required_string(accepted, "network", "paymentPayload.accepted.network")?;
    required_string(accepted, "asset", "paymentPayload.accepted.asset")?;
    required_decimal_string(accepted, "amount", "paymentPayload.accepted.amount")?;
    required_string(accepted, "payTo", "paymentPayload.accepted.payTo")?;

    let payment_identifier = extract_payment_identifier(payload, identifier_policy)?;
    let raw = proto::VerifyRequest::from(serde_json::value::to_raw_value(&value)?);
    Ok(ParsedRequest {
        raw,
        value,
        meta: RequestMeta {
            x402_version,
            scheme,
            network,
            asset,
            amount,
            pay_to,
            signed_delegate_action,
            payment_identifier,
        },
    })
}

pub fn request_fingerprint(
    request: &Value,
    payment_hash: &[u8; 32],
) -> Result<[u8; 32], serde_json::Error> {
    let mut canonical = Vec::new();
    write_canonical_json(request, &mut canonical)?;
    let mut hash = Sha256::new();
    hash.update(REQUEST_FINGERPRINT_DOMAIN);
    hash.update(canonical);
    hash.update(payment_hash);
    Ok(hash.finalize().into())
}

pub fn decimal_is_at_least(value: &str, minimum: &str) -> bool {
    compare_decimal(value, minimum) != Ordering::Less
}

pub fn validate_identifier(
    identifier: &str,
    policy: &PaymentIdentifierConfig,
) -> Result<(), RequestError> {
    if identifier.len() < policy.min_length || identifier.len() > policy.max_length {
        return Err(RequestError::PaymentIdentifier(format!(
            "id length must be {}..={}",
            policy.min_length, policy.max_length
        )));
    }
    if !identifier
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        return Err(RequestError::PaymentIdentifier(
            "id contains characters outside [A-Za-z0-9_-]".to_owned(),
        ));
    }
    Ok(())
}

fn extract_payment_identifier(
    payload: &Map<String, Value>,
    policy: &PaymentIdentifierConfig,
) -> Result<Option<String>, RequestError> {
    let extension = payload
        .get("extensions")
        .and_then(Value::as_object)
        .and_then(|extensions| extensions.get(PAYMENT_IDENTIFIER_EXTENSION));
    let Some(extension) = extension else {
        if policy.required {
            return Err(RequestError::PaymentIdentifier(
                "an id is required by this facilitator".to_owned(),
            ));
        }
        return Ok(None);
    };
    let extension = extension
        .as_object()
        .ok_or_else(|| RequestError::PaymentIdentifier("extension must be an object".to_owned()))?;
    ensure_allowed_keys(
        extension,
        &["info", "schema"],
        "paymentPayload.extensions.payment-identifier",
    )?;
    let info = extension
        .get("info")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            RequestError::PaymentIdentifier("extension.info must be an object".to_owned())
        })?;
    ensure_allowed_keys(
        info,
        &["required", "id"],
        "paymentPayload.extensions.payment-identifier.info",
    )?;
    if info.get("required").and_then(Value::as_bool).is_none() {
        return Err(RequestError::PaymentIdentifier(
            "extension.info.required must be a boolean".to_owned(),
        ));
    }
    if !extension.get("schema").is_some_and(Value::is_object) {
        return Err(RequestError::PaymentIdentifier(
            "extension.schema must be an object".to_owned(),
        ));
    }
    let identifier = info.get("id");
    match identifier {
        Some(Value::String(identifier)) => {
            validate_identifier(identifier, policy)?;
            Ok(Some(identifier.clone()))
        }
        Some(_) => Err(RequestError::PaymentIdentifier(
            "extension.info.id must be a string".to_owned(),
        )),
        None if policy.required => Err(RequestError::PaymentIdentifier(
            "an id is required by this facilitator".to_owned(),
        )),
        None => Ok(None),
    }
}

fn validate_requirements_shape(
    object: &Map<String, Value>,
    path: &'static str,
) -> Result<(), RequestError> {
    ensure_allowed_keys(
        object,
        &[
            "scheme",
            "network",
            "asset",
            "amount",
            "payTo",
            "maxTimeoutSeconds",
            "extra",
        ],
        path,
    )?;
    if object
        .get("maxTimeoutSeconds")
        .and_then(Value::as_u64)
        .filter(|timeout| *timeout > 0)
        .is_none()
    {
        return Err(RequestError::Field(path));
    }
    if object.get("extra").is_some_and(|extra| !extra.is_object()) {
        return Err(RequestError::Field(path));
    }
    Ok(())
}

fn validate_resource(resource: Option<&Value>) -> Result<(), RequestError> {
    let Some(resource) = resource else {
        return Ok(());
    };
    let resource = resource
        .as_object()
        .ok_or(RequestError::Field("paymentPayload.resource"))?;
    ensure_allowed_keys(
        resource,
        &[
            "url",
            "description",
            "mimeType",
            "serviceName",
            "tags",
            "iconUrl",
        ],
        "paymentPayload.resource",
    )?;
    required_string(resource, "url", "paymentPayload.resource.url")?;
    for field in ["description", "mimeType", "serviceName", "iconUrl"] {
        if resource.get(field).is_some_and(|value| !value.is_string()) {
            return Err(RequestError::Field("paymentPayload.resource"));
        }
    }
    if let Some(tags) = resource.get("tags") {
        let tags = tags
            .as_array()
            .ok_or(RequestError::Field("paymentPayload.resource.tags"))?;
        if tags.iter().any(|tag| !tag.is_string()) {
            return Err(RequestError::Field("paymentPayload.resource.tags"));
        }
    }
    Ok(())
}

fn ensure_allowed_keys(
    object: &Map<String, Value>,
    allowed: &[&str],
    path: &'static str,
) -> Result<(), RequestError> {
    if object
        .keys()
        .any(|key| !allowed.iter().any(|allowed| key == allowed))
    {
        return Err(RequestError::Field(path));
    }
    Ok(())
}

fn required_u8(
    object: &Map<String, Value>,
    key: &'static str,
    path: &'static str,
) -> Result<u8, RequestError> {
    object
        .get(key)
        .and_then(Value::as_u64)
        .and_then(|value| u8::try_from(value).ok())
        .ok_or(RequestError::Field(path))
}

fn required_string(
    object: &Map<String, Value>,
    key: &'static str,
    path: &'static str,
) -> Result<String, RequestError> {
    object
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .ok_or(RequestError::Field(path))
}

fn required_decimal_string(
    object: &Map<String, Value>,
    key: &'static str,
    path: &'static str,
) -> Result<String, RequestError> {
    let value = required_string(object, key, path)?;
    if value.bytes().all(|byte| byte.is_ascii_digit()) {
        Ok(value)
    } else {
        Err(RequestError::Field(path))
    }
}

fn compare_decimal(left: &str, right: &str) -> Ordering {
    let left = left.trim_start_matches('0');
    let right = right.trim_start_matches('0');
    let left = if left.is_empty() { "0" } else { left };
    let right = if right.is_empty() { "0" } else { right };
    left.len()
        .cmp(&right.len())
        .then_with(|| left.as_bytes().cmp(right.as_bytes()))
}

fn write_canonical_json(value: &Value, output: &mut Vec<u8>) -> Result<(), serde_json::Error> {
    match value {
        Value::Null => output.extend_from_slice(b"null"),
        Value::Bool(true) => output.extend_from_slice(b"true"),
        Value::Bool(false) => output.extend_from_slice(b"false"),
        Value::Number(number) => output.extend_from_slice(number.to_string().as_bytes()),
        Value::String(string) => {
            output.extend_from_slice(serde_json::to_string(string)?.as_bytes());
        }
        Value::Array(values) => {
            output.push(b'[');
            for (index, value) in values.iter().enumerate() {
                if index != 0 {
                    output.push(b',');
                }
                write_canonical_json(value, output)?;
            }
            output.push(b']');
        }
        Value::Object(object) => {
            output.push(b'{');
            let mut keys: Vec<&String> = object.keys().collect();
            keys.sort_unstable();
            for (index, key) in keys.into_iter().enumerate() {
                if index != 0 {
                    output.push(b',');
                }
                output.extend_from_slice(serde_json::to_string(key)?.as_bytes());
                output.push(b':');
                if let Some(child) = object.get(key) {
                    write_canonical_json(child, output)?;
                }
            }
            output.push(b'}');
        }
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredProtocolResponse {
    pub success: Option<bool>,
    pub is_valid: Option<bool>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request_value() -> Value {
        serde_json::json!({
            "x402Version": 2,
            "paymentPayload": {
                "x402Version": 2,
                "accepted": {
                    "scheme": "exact",
                    "network": "near:testnet",
                    "asset": "usdc.fakes.testnet",
                    "amount": "1000",
                    "payTo": "merchant.testnet",
                    "maxTimeoutSeconds": 60,
                    "extra": {},
                },
                "payload": {
                    "signedDelegateAction": "c2lnbmVkLWRlbGVnYXRl",
                },
                "extensions": {
                    "payment-identifier": {
                        "info": {
                            "required": true,
                            "id": "payment_1234567890123456",
                        },
                        "schema": {},
                    },
                },
            },
            "paymentRequirements": {
                "scheme": "exact",
                "network": "near:testnet",
                "asset": "usdc.fakes.testnet",
                "amount": "1000",
                "payTo": "merchant.testnet",
                "maxTimeoutSeconds": 60,
                "extra": {},
            },
        })
    }

    fn encoded(value: &Value) -> Vec<u8> {
        serde_json::to_vec(value).unwrap_or_else(|_| std::process::abort())
    }

    #[test]
    fn identifier_validation_matches_extension_spec() {
        let policy = PaymentIdentifierConfig::default();
        assert!(validate_identifier("pay_123456789012", &policy).is_ok());
        assert!(validate_identifier("too-short", &policy).is_err());
        assert!(validate_identifier("pay_1234567890/$", &policy).is_err());
    }

    #[test]
    fn canonical_fingerprint_ignores_object_key_order() {
        let left = serde_json::json!({"b": 2, "a": {"d": 4, "c": 3}});
        let right = serde_json::json!({"a": {"c": 3, "d": 4}, "b": 2});
        let payment_hash = [7_u8; 32];
        assert_eq!(
            request_fingerprint(&left, &payment_hash).ok(),
            request_fingerprint(&right, &payment_hash).ok()
        );
    }

    #[test]
    fn canonical_json_is_valid_and_has_sorted_object_keys() {
        let mut output = Vec::new();
        assert!(write_canonical_json(&serde_json::json!({"b": 2, "a": 1}), &mut output).is_ok());
        assert_eq!(output, br#"{"a":1,"b":2}"#);
        assert!(serde_json::from_slice::<Value>(&output).is_ok());
    }

    #[test]
    fn request_shape_rejects_unknown_and_missing_nested_fields() {
        let policy = PaymentIdentifierConfig::default();
        let valid = request_value();
        assert!(parse_request(&encoded(&valid), &policy).is_ok());

        let mut unknown_top = valid.clone();
        unknown_top["unexpected"] = Value::Bool(true);
        assert!(parse_request(&encoded(&unknown_top), &policy).is_err());

        let mut unknown_payload = valid.clone();
        unknown_payload["paymentPayload"]["unexpected"] = Value::Bool(true);
        assert!(parse_request(&encoded(&unknown_payload), &policy).is_err());

        let mut missing_nested_version = valid;
        if let Some(payload) = missing_nested_version
            .get_mut("paymentPayload")
            .and_then(Value::as_object_mut)
        {
            payload.remove("x402Version");
        }
        assert!(parse_request(&encoded(&missing_nested_version), &policy).is_err());
    }

    #[test]
    fn decimal_comparison_is_numeric() {
        assert!(decimal_is_at_least("001000", "1000"));
        assert!(!decimal_is_at_least("999", "1000"));
        assert!(decimal_is_at_least("1000000000000000000000000", "1000"));
    }

    #[test]
    fn settlement_failure_always_serializes_transaction_and_network() {
        let response = SettleResponse::failure(
            "settlement_failed",
            None,
            None,
            String::new(),
            "near:testnet".to_owned(),
        );
        let value = serde_json::to_value(response).ok();
        assert_eq!(
            value.as_ref().and_then(|value| value.get("transaction")),
            Some(&Value::String(String::new()))
        );
        assert_eq!(
            value.as_ref().and_then(|value| value.get("network")),
            Some(&Value::String("near:testnet".to_owned()))
        );
    }

    #[test]
    fn request_metadata_debug_output_redacts_payment_material() {
        let meta = RequestMeta {
            x402_version: 2,
            scheme: "exact".to_owned(),
            network: "near:testnet".to_owned(),
            asset: "usdc.testnet".to_owned(),
            amount: "1000".to_owned(),
            pay_to: "sensitive-payee.testnet".to_owned(),
            signed_delegate_action: "sensitive-signed-delegate".to_owned(),
            payment_identifier: Some("sensitive_identifier_1234".to_owned()),
        };
        let debug = format!("{meta:?}");
        assert!(debug.contains("<redacted>"));
        assert!(!debug.contains("sensitive-payee.testnet"));
        assert!(!debug.contains("sensitive-signed-delegate"));
        assert!(!debug.contains("sensitive_identifier_1234"));
    }
}
