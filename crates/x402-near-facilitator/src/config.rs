use std::env;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use url::Url;
use zeroize::Zeroizing;

const MAINNET: &str = "near:mainnet";
const TESTNET: &str = "near:testnet";

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Environment {
    Mainnet,
    Testnet,
}

impl Environment {
    pub const fn network(self) -> &'static str {
        match self {
            Self::Mainnet => MAINNET,
            Self::Testnet => TESTNET,
        }
    }

    pub const fn api_key_label(self) -> &'static str {
        match self {
            Self::Mainnet => "live",
            Self::Testnet => "test",
        }
    }

    pub const fn default_client_budget(self) -> &'static str {
        match self {
            // 0.10 NEAR and 1 NEAR, respectively.
            Self::Mainnet => "100000000000000000000000",
            Self::Testnet => "1000000000000000000000000",
        }
    }
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(missing_debug_implementations)]
pub struct ServiceConfig {
    pub environment: Environment,
    pub network: String,
    pub bind_address: SocketAddr,
    pub primary_rpc_url: Url,
    pub backup_rpc_url: Url,
    pub asset: String,
    pub asset_symbol: String,
    pub minimum_amount: String,
    pub relayer_account_id: String,
    pub max_inner_gas: u64,
    #[serde(default = "default_database_connections")]
    pub database_max_connections: u32,
    #[serde(default)]
    pub request_limits: RequestLimits,
    pub sponsorship: SponsorshipConfig,
    #[serde(default)]
    pub payment_identifier: PaymentIdentifierConfig,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RequestLimits {
    pub body_bytes: usize,
    pub verify_per_minute: u32,
    pub settle_per_minute: u32,
    pub verify_timeout_seconds: u64,
    pub settle_timeout_seconds: u64,
    pub max_concurrent_verify: usize,
}

impl Default for RequestLimits {
    fn default() -> Self {
        Self {
            body_bytes: 65_536,
            verify_per_minute: 60,
            settle_per_minute: 10,
            verify_timeout_seconds: 15,
            settle_timeout_seconds: 60,
            max_concurrent_verify: 64,
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SponsorshipConfig {
    pub global_daily_yocto_near: String,
    pub default_client_daily_yocto_near: String,
    pub reservation_yocto_near: String,
    pub balance_warning_yocto_near: String,
    pub balance_hard_stop_yocto_near: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PaymentIdentifierConfig {
    pub required: bool,
    pub min_length: usize,
    pub max_length: usize,
}

impl Default for PaymentIdentifierConfig {
    fn default() -> Self {
        Self {
            required: false,
            min_length: 16,
            max_length: 128,
        }
    }
}

#[derive(Clone)]
#[allow(missing_debug_implementations)]
pub struct SecretFiles {
    pub database_url: PathBuf,
    pub database_direct_url: PathBuf,
    pub relayer_key: PathBuf,
    pub api_key_pepper: PathBuf,
}

#[allow(missing_debug_implementations)]
pub struct RuntimeSecrets {
    pub database_url: Zeroizing<String>,
    pub database_direct_url: Zeroizing<String>,
    pub relayer_key: Zeroizing<String>,
    pub api_key_pepper: Zeroizing<String>,
}

#[derive(Clone)]
#[allow(missing_debug_implementations)]
pub struct OtelConfig {
    pub endpoint: Url,
    pub headers_file: PathBuf,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("failed to read config file: {0}")]
    Read(#[from] std::io::Error),
    #[error("invalid config JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("missing required environment variable {0}")]
    MissingEnvironment(&'static str),
    #[error("invalid environment variable {name}: {message}")]
    Environment { name: &'static str, message: String },
    #[error("invalid configuration: {0}")]
    Invalid(String),
    #[error("secret file {path} has group or world permissions")]
    InsecureSecretMode { path: PathBuf },
    #[error("secret path {path} must be a regular file, not a symlink or special file")]
    InvalidSecretFileType { path: PathBuf },
}

impl ServiceConfig {
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let bytes = std::fs::read(path)?;
        let config: Self = serde_json::from_slice(&bytes)?;
        config.validate()?;
        Ok(config)
    }

    // Keep launch-policy validation in one ordered, auditable sequence.
    #[allow(clippy::too_many_lines)]
    pub fn validate(&self) -> Result<(), ConfigError> {
        const MAX_INNER_GAS: u64 = 30_000_000_000_000;
        const MAINNET_USDC: &str =
            "17208628f84f5d6ad33f0da3bbbeb27ffcb398eac501a31bd6ad2011e36133a1";
        const TESTNET_USDC: &str =
            "3e2210e1184b45b64c8a434c0a7e7b23cc04ea7eb7a6c3c32520d03d4afcb8af";

        if self.network != self.environment.network() {
            return Err(ConfigError::Invalid(format!(
                "network {} does not match environment {:?}",
                self.network, self.environment
            )));
        }
        if self.primary_rpc_url == self.backup_rpc_url {
            return Err(ConfigError::Invalid(
                "primary_rpc_url and backup_rpc_url must be independent".to_owned(),
            ));
        }
        if self.primary_rpc_url.host_str() == self.backup_rpc_url.host_str() {
            return Err(ConfigError::Invalid(
                "primary_rpc_url and backup_rpc_url must use different hosts".to_owned(),
            ));
        }
        if self.primary_rpc_url.scheme() != "https" || self.backup_rpc_url.scheme() != "https" {
            return Err(ConfigError::Invalid(
                "production RPC URLs must use HTTPS".to_owned(),
            ));
        }
        if !self.bind_address.ip().is_loopback() {
            return Err(ConfigError::Invalid(
                "bind_address must be loopback; expose the service through the hardened proxy"
                    .to_owned(),
            ));
        }
        for (name, value) in [
            ("minimum_amount", self.minimum_amount.as_str()),
            (
                "sponsorship.global_daily_yocto_near",
                self.sponsorship.global_daily_yocto_near.as_str(),
            ),
            (
                "sponsorship.default_client_daily_yocto_near",
                self.sponsorship.default_client_daily_yocto_near.as_str(),
            ),
            (
                "sponsorship.reservation_yocto_near",
                self.sponsorship.reservation_yocto_near.as_str(),
            ),
            (
                "sponsorship.balance_warning_yocto_near",
                self.sponsorship.balance_warning_yocto_near.as_str(),
            ),
            (
                "sponsorship.balance_hard_stop_yocto_near",
                self.sponsorship.balance_hard_stop_yocto_near.as_str(),
            ),
        ] {
            validate_unsigned_decimal(name, value)?;
        }
        let expected_asset = match self.environment {
            Environment::Mainnet => MAINNET_USDC,
            Environment::Testnet => TESTNET_USDC,
        };
        if self.asset != expected_asset {
            return Err(ConfigError::Invalid(format!(
                "asset must be the canonical Circle USDC contract for {}",
                self.network
            )));
        }
        if self.asset_symbol != "USDC" {
            return Err(ConfigError::Invalid(
                "asset_symbol must be USDC for the launch policy".to_owned(),
            ));
        }
        self.asset
            .parse::<near_primitives::types::AccountId>()
            .map_err(|error| ConfigError::Invalid(format!("invalid asset account ID: {error}")))?;
        self.relayer_account_id
            .parse::<near_primitives::types::AccountId>()
            .map_err(|error| {
                ConfigError::Invalid(format!("invalid relayer_account_id: {error}"))
            })?;
        if self.max_inner_gas != MAX_INNER_GAS {
            return Err(ConfigError::Invalid(
                "max_inner_gas must be exactly 30000000000000 for the launch policy".to_owned(),
            ));
        }
        if compare_decimal(&self.minimum_amount, "1000").is_lt() {
            return Err(ConfigError::Invalid(
                "minimum_amount must be at least 1000 atomic USDC".to_owned(),
            ));
        }
        if !compare_decimal(
            &self.sponsorship.balance_hard_stop_yocto_near,
            &self.sponsorship.balance_warning_yocto_near,
        )
        .is_lt()
        {
            return Err(ConfigError::Invalid(
                "sponsorship.balance_hard_stop_yocto_near must be below the warning threshold"
                    .to_owned(),
            ));
        }
        if compare_decimal(
            &self.sponsorship.reservation_yocto_near,
            &self.sponsorship.default_client_daily_yocto_near,
        )
        .is_gt()
            || compare_decimal(
                &self.sponsorship.reservation_yocto_near,
                &self.sponsorship.global_daily_yocto_near,
            )
            .is_gt()
        {
            return Err(ConfigError::Invalid(
                "sponsorship reservation must not exceed either daily cap".to_owned(),
            ));
        }
        if self.request_limits.body_bytes != 65_536 {
            return Err(ConfigError::Invalid(
                "request_limits.body_bytes must be exactly 65536".to_owned(),
            ));
        }
        if self.database_max_connections == 0
            || self.request_limits.verify_per_minute == 0
            || self.request_limits.settle_per_minute == 0
            || self.request_limits.verify_timeout_seconds == 0
            || self.request_limits.settle_timeout_seconds == 0
            || self.request_limits.max_concurrent_verify == 0
        {
            return Err(ConfigError::Invalid(
                "database and request limits must be positive".to_owned(),
            ));
        }
        if self.payment_identifier.min_length != 16 || self.payment_identifier.max_length != 128 {
            return Err(ConfigError::Invalid(
                "payment_identifier bounds must match the x402 extension (16..=128)".to_owned(),
            ));
        }
        Ok(())
    }

    pub fn policy_snapshot(&self) -> serde_json::Value {
        serde_json::json!({
            "network": self.network,
            "asset": self.asset,
            "minimumAmount": self.minimum_amount,
            "maxInnerGas": self.max_inner_gas,
            "reservationYoctoNear": self.sponsorship.reservation_yocto_near,
        })
    }
}

impl SecretFiles {
    pub fn from_environment() -> Result<Self, ConfigError> {
        let database_url = required_path("DATABASE_URL_FILE")?;
        let database_direct_url =
            optional_path("DATABASE_DIRECT_URL_FILE").unwrap_or_else(|| database_url.clone());
        Ok(Self {
            database_url,
            database_direct_url,
            relayer_key: required_path("RELAYER_KEY_FILE")?,
            api_key_pepper: required_path("API_KEY_PEPPER_FILE")?,
        })
    }

    pub fn load(&self) -> Result<RuntimeSecrets, ConfigError> {
        Ok(RuntimeSecrets {
            database_url: read_secret(&self.database_url)?,
            database_direct_url: read_secret(&self.database_direct_url)?,
            relayer_key: read_secret(&self.relayer_key)?,
            api_key_pepper: read_secret(&self.api_key_pepper)?,
        })
    }
}

impl OtelConfig {
    pub fn from_environment() -> Result<Option<Self>, ConfigError> {
        let endpoint = env::var("OTEL_EXPORTER_OTLP_ENDPOINT").ok();
        let headers_file = optional_path("OTEL_EXPORTER_OTLP_HEADERS_FILE");
        match (endpoint, headers_file) {
            (None, None) => Ok(None),
            (Some(endpoint), Some(headers_file)) => {
                let endpoint = Url::parse(&endpoint).map_err(|error| ConfigError::Environment {
                    name: "OTEL_EXPORTER_OTLP_ENDPOINT",
                    message: error.to_string(),
                })?;
                validate_otel_endpoint(&endpoint)?;
                Ok(Some(Self {
                    endpoint,
                    headers_file,
                }))
            }
            _ => Err(ConfigError::Invalid(
                "OTLP is enabled only when both OTEL_EXPORTER_OTLP_ENDPOINT and \
                 OTEL_EXPORTER_OTLP_HEADERS_FILE are set"
                    .to_owned(),
            )),
        }
    }
}

fn validate_otel_endpoint(endpoint: &Url) -> Result<(), ConfigError> {
    if endpoint.scheme() != "https" {
        return Err(ConfigError::Invalid(
            "OTEL_EXPORTER_OTLP_ENDPOINT must use HTTPS".to_owned(),
        ));
    }
    if endpoint.path() != "/"
        || endpoint.query().is_some()
        || endpoint.fragment().is_some()
        || !endpoint.username().is_empty()
        || endpoint.password().is_some()
    {
        return Err(ConfigError::Invalid(
            "OTEL_EXPORTER_OTLP_ENDPOINT must be an HTTPS origin without \
             credentials, a path, query, or fragment"
                .to_owned(),
        ));
    }
    Ok(())
}

pub fn read_secret(path: &Path) -> Result<Zeroizing<String>, ConfigError> {
    ensure_private_mode(path)?;
    let bytes = std::fs::read(path)?;
    if bytes.len() > 65_536 {
        return Err(ConfigError::Invalid(format!(
            "secret file {} is unexpectedly large",
            path.display()
        )));
    }
    let value = String::from_utf8(bytes).map_err(|error| {
        ConfigError::Invalid(format!(
            "secret file {} is not UTF-8: {error}",
            path.display()
        ))
    })?;
    let value = if let Some(value) = value.strip_suffix("\r\n") {
        value
    } else if let Some(value) = value.strip_suffix('\n') {
        value
    } else {
        return Err(ConfigError::Invalid(format!(
            "secret file {} must end with exactly one newline",
            path.display()
        )));
    };
    if value.is_empty() || value.chars().any(char::is_whitespace) {
        return Err(ConfigError::Invalid(format!(
            "secret file {} is empty or contains non-terminal whitespace",
            path.display()
        )));
    }
    Ok(Zeroizing::new(value.to_owned()))
}

fn required_path(name: &'static str) -> Result<PathBuf, ConfigError> {
    optional_path(name).ok_or(ConfigError::MissingEnvironment(name))
}

fn optional_path(name: &'static str) -> Option<PathBuf> {
    env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

#[cfg(unix)]
fn ensure_private_mode(path: &Path) -> Result<(), ConfigError> {
    use std::os::unix::fs::PermissionsExt;

    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.file_type().is_file() {
        return Err(ConfigError::InvalidSecretFileType {
            path: path.to_owned(),
        });
    }
    // systemd's LoadCredential materializes secrets under $CREDENTIALS_DIRECTORY
    // as 0440 root:root guarded by a POSIX ACL that grants read access only to
    // the service user. stat's group bits then report the ACL mask rather than
    // real group access, so the raw-mode check below would reject a file that
    // is in fact isolated to this process. Trust systemd's own credentials
    // directory; every other path stays strictly owner-only.
    if is_within_systemd_credentials(path) {
        return Ok(());
    }
    let mode = metadata.permissions().mode();
    if mode & 0o077 != 0 {
        return Err(ConfigError::InsecureSecretMode {
            path: path.to_owned(),
        });
    }
    Ok(())
}

#[cfg(unix)]
fn is_within_systemd_credentials(path: &Path) -> bool {
    // $CREDENTIALS_DIRECTORY is set only by systemd and always points at the
    // per-unit ramfs mount that holds LoadCredential secrets.
    match env::var_os("CREDENTIALS_DIRECTORY") {
        Some(dir) => path_is_within(&PathBuf::from(dir), path),
        None => false,
    }
}

#[cfg(unix)]
fn path_is_within(directory: &Path, path: &Path) -> bool {
    !directory.as_os_str().is_empty() && path.starts_with(directory)
}

#[cfg(not(unix))]
fn ensure_private_mode(_path: &Path) -> Result<(), ConfigError> {
    Ok(())
}

fn validate_unsigned_decimal(name: &str, value: &str) -> Result<(), ConfigError> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(ConfigError::Invalid(format!(
            "{name} must be an unsigned base-10 integer string"
        )));
    }
    Ok(())
}

fn compare_decimal(left: &str, right: &str) -> std::cmp::Ordering {
    let left = left.trim_start_matches('0');
    let right = right.trim_start_matches('0');
    let left = if left.is_empty() { "0" } else { left };
    let right = if right.is_empty() { "0" } else { right };
    left.len()
        .cmp(&right.len())
        .then_with(|| left.as_bytes().cmp(right.as_bytes()))
}

const fn default_database_connections() -> u32 {
    10
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_mainnet_config() -> ServiceConfig {
        ServiceConfig {
            environment: Environment::Mainnet,
            network: MAINNET.to_owned(),
            bind_address: SocketAddr::from(([127, 0, 0, 1], 8402)),
            primary_rpc_url: Url::parse("https://rpc.mainnet.fastnear.com")
                .unwrap_or_else(|_| std::process::abort()),
            backup_rpc_url: Url::parse("https://rpc.mainnet.near.org")
                .unwrap_or_else(|_| std::process::abort()),
            asset: "17208628f84f5d6ad33f0da3bbbeb27ffcb398eac501a31bd6ad2011e36133a1".to_owned(),
            asset_symbol: "USDC".to_owned(),
            minimum_amount: "1000".to_owned(),
            relayer_account_id: "x402-relayer.mike.near".to_owned(),
            max_inner_gas: 30_000_000_000_000,
            database_max_connections: 10,
            request_limits: RequestLimits::default(),
            sponsorship: SponsorshipConfig {
                global_daily_yocto_near: "500000000000000000000000".to_owned(),
                default_client_daily_yocto_near: "100000000000000000000000".to_owned(),
                reservation_yocto_near: "10000000000000000000000".to_owned(),
                balance_warning_yocto_near: "1000000000000000000000000".to_owned(),
                balance_hard_stop_yocto_near: "250000000000000000000000".to_owned(),
            },
            payment_identifier: PaymentIdentifierConfig::default(),
        }
    }

    #[test]
    fn accepts_launch_mainnet_policy() {
        assert!(valid_mainnet_config().validate().is_ok());
    }

    #[test]
    fn rejects_network_environment_mismatch() {
        let mut config = valid_mainnet_config();
        config.network = TESTNET.to_owned();
        assert!(config.validate().is_err());
    }

    #[test]
    fn rejects_non_loopback_bind() {
        let mut config = valid_mainnet_config();
        config.bind_address = SocketAddr::from(([0, 0, 0, 0], 8402));
        assert!(config.validate().is_err());
    }

    #[test]
    fn rejects_noncanonical_asset_or_gas() {
        let mut asset = valid_mainnet_config();
        asset.asset = "usdc.near".to_owned();
        assert!(asset.validate().is_err());

        let mut gas = valid_mainnet_config();
        gas.max_inner_gas += 1;
        assert!(gas.validate().is_err());
    }

    #[test]
    fn rejects_unsafe_sponsorship_thresholds() {
        let mut config = valid_mainnet_config();
        config.sponsorship.balance_hard_stop_yocto_near =
            config.sponsorship.balance_warning_yocto_near.clone();
        assert!(config.validate().is_err());

        let mut config = valid_mainnet_config();
        config.sponsorship.reservation_yocto_near = "600000000000000000000000".to_owned();
        assert!(config.validate().is_err());
    }

    #[test]
    fn rejects_same_rpc_host() {
        let mut config = valid_mainnet_config();
        config.backup_rpc_url = Url::parse("https://rpc.mainnet.fastnear.com/backup")
            .unwrap_or_else(|_| std::process::abort());
        assert!(config.validate().is_err());
    }

    #[test]
    fn request_body_limit_is_fixed_at_64_kib() {
        let mut config = valid_mainnet_config();
        config.request_limits.body_bytes = 65_537;
        assert!(config.validate().is_err());
        config.request_limits.body_bytes = 65_535;
        assert!(config.validate().is_err());
        config.request_limits.body_bytes = 65_536;
        assert!(config.validate().is_ok());
    }

    #[test]
    fn payment_identifier_bounds_are_fixed_by_spec() {
        let config = PaymentIdentifierConfig::default();
        assert_eq!(config.min_length, 16);
        assert_eq!(config.max_length, 128);
    }

    #[test]
    fn otlp_endpoint_is_an_https_origin() {
        let valid =
            Url::parse("https://api.honeycomb.io").unwrap_or_else(|_| std::process::abort());
        assert!(validate_otel_endpoint(&valid).is_ok());
        for invalid in [
            "http://api.honeycomb.io",
            "https://api.honeycomb.io/v1/traces",
            "https://api.honeycomb.io?signal=traces",
            "https://user:password@api.honeycomb.io",
        ] {
            let invalid = Url::parse(invalid).unwrap_or_else(|_| std::process::abort());
            assert!(validate_otel_endpoint(&invalid).is_err());
        }
    }

    #[test]
    fn secret_files_require_exactly_one_terminal_line_ending() {
        for accepted in [b"secret\n".as_slice(), b"secret\r\n".as_slice()] {
            let path = private_test_file(accepted);
            assert_eq!(
                read_secret(&path).ok().as_deref().map(String::as_str),
                Some("secret")
            );
            let _remove = std::fs::remove_file(path);
        }
        for rejected in [
            b"secret".as_slice(),
            b"secret\n\n".as_slice(),
            b"secret\r\n\r\n".as_slice(),
            b" secret\n".as_slice(),
            b"secret \n".as_slice(),
            b"sec\tret\n".as_slice(),
        ] {
            let path = private_test_file(rejected);
            assert!(read_secret(&path).is_err());
            let _remove = std::fs::remove_file(path);
        }
    }

    #[test]
    fn systemd_credentials_directory_scopes_the_mode_exception() {
        let creds = Path::new("/run/credentials/x402-near-facilitator@testnet.service");
        assert!(path_is_within(creds, &creds.join("relayer-key")));
        // An empty $CREDENTIALS_DIRECTORY must never disable the mode check.
        assert!(!path_is_within(Path::new(""), Path::new("/run/credentials/x/relayer-key")));
        // A sibling path that merely shares a name prefix is not inside.
        assert!(!path_is_within(
            creds,
            Path::new("/run/credentials/x402-near-facilitator@testnet.service.bak/relayer-key")
        ));
        // Unrelated absolute secrets stay outside the exception.
        assert!(!path_is_within(creds, Path::new("/etc/x402-near-facilitator/mainnet.json")));
    }

    fn private_test_file(bytes: &[u8]) -> PathBuf {
        use std::os::unix::fs::PermissionsExt as _;

        let path =
            std::env::temp_dir().join(format!("x402-near-secret-test-{}", uuid::Uuid::new_v4()));
        std::fs::write(&path, bytes).unwrap_or_else(|_| std::process::abort());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .unwrap_or_else(|_| std::process::abort());
        path
    }
}
