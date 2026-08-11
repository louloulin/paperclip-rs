//! 决策规格签名与实例级密钥管理。
//!
//! 对齐 Node `services/decision-signing.ts`：canonical JSON、HMAC-SHA256、
//! 显式环境密钥校验，以及并发安全的本地密钥生成与权限修复。

mod canonical;
mod key_store;

use hmac::{Hmac, Mac};
use serde_json::Value;
use sha2::Sha256;
use std::env;
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;

pub use canonical::{canonical, canonical_number};
pub use key_store::DecisionSigningKeyStore;

pub const DECISION_SIGNING_VERSION: &str = "decision-spec-v1";
pub const MIN_DECISION_SIGNING_SECRET_LENGTH: usize = 32;

#[derive(Clone)]
pub struct DecisionSigningService {
    source: DecisionSigningSecretSource,
}

#[derive(Clone)]
enum DecisionSigningSecretSource {
    Environment,
    Fixed(Arc<str>),
}

impl DecisionSigningService {
    pub fn from_environment() -> Self {
        Self {
            source: DecisionSigningSecretSource::Environment,
        }
    }

    pub fn from_secret(secret: &str) -> Result<Self, DecisionSigningError> {
        let secret = validated_secret(secret)?;
        Ok(Self {
            source: DecisionSigningSecretSource::Fixed(Arc::from(secret)),
        })
    }

    pub fn sign(&self, value: &Value) -> Result<String, DecisionSigningError> {
        match &self.source {
            DecisionSigningSecretSource::Environment => sign_decision_spec(value),
            DecisionSigningSecretSource::Fixed(secret) => {
                sign_decision_spec_with_secret(value, secret)
            }
        }
    }

    pub fn verify(&self, value: &Value, signature: &str) -> Result<bool, DecisionSigningError> {
        match &self.source {
            DecisionSigningSecretSource::Environment => verify_decision_spec(value, signature),
            DecisionSigningSecretSource::Fixed(secret) => {
                verify_decision_spec_with_secret(value, signature, secret)
            }
        }
    }
}

impl Default for DecisionSigningService {
    fn default() -> Self {
        Self::from_environment()
    }
}

impl fmt::Debug for DecisionSigningService {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let source = match self.source {
            DecisionSigningSecretSource::Environment => "environment_or_file",
            DecisionSigningSecretSource::Fixed(_) => "fixed_redacted",
        };
        formatter
            .debug_struct("DecisionSigningService")
            .field("source", &source)
            .finish()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DecisionSigningError {
    #[error(transparent)]
    HomePath(#[from] pc_config::HomePathError),
    #[error("PAPERCLIP_DECISION_SIGNING_SECRET contains non-Unicode data")]
    NonUnicodeEnvironmentSecret,
    #[error(
        "PAPERCLIP_DECISION_SIGNING_SECRET must be at least 32 characters when set (unset it to use an auto-generated key)"
    )]
    ExplicitSecretTooShort,
    #[error(
        "Invalid decision signing key at {} (must be at least 32 characters); remove the file to regenerate it or set PAPERCLIP_DECISION_SIGNING_SECRET",
        path.display()
    )]
    GeneratedSecretTooShort { path: PathBuf },
    #[error("Decision signing key at {} must be a regular file", path.display())]
    KeyNotRegularFile { path: PathBuf },
    #[error(
        "Decision signing secrets directory at {} must be a directory",
        path.display()
    )]
    SecretsPathNotDirectory { path: PathBuf },
    #[error("{description} must be owned by the Paperclip process user")]
    WrongOwner { description: String },
    #[error("Decision signing key at {} must have permissions 0600", path.display())]
    KeyPermissions { path: PathBuf },
    #[error(
        "Decision signing secrets directory at {} must have permissions 0700",
        path.display()
    )]
    DirectoryPermissions { path: PathBuf },
    #[error("decision signing key path has no parent: {}", path.display())]
    MissingParentDirectory { path: PathBuf },
    #[error(
        "decision signing I/O failed while {operation} {}: {source}",
        path.display()
    )]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

impl DecisionSigningError {
    pub(crate) fn io(
        operation: &'static str,
        path: impl Into<PathBuf>,
        source: std::io::Error,
    ) -> Self {
        Self::Io {
            operation,
            path: path.into(),
            source,
        }
    }

    pub(crate) fn is_not_found(&self) -> bool {
        matches!(
            self,
            Self::Io { source, .. } if source.kind() == std::io::ErrorKind::NotFound
        )
    }
}

pub fn resolve_decision_signing_secret() -> Result<String, DecisionSigningError> {
    let explicit_secret = env::var_os("PAPERCLIP_DECISION_SIGNING_SECRET")
        .map(|value| {
            value
                .into_string()
                .map_err(|_| DecisionSigningError::NonUnicodeEnvironmentSecret)
        })
        .transpose()?;
    DecisionSigningKeyStore::from_env()?.resolve_secret(explicit_secret.as_deref())
}

pub fn ensure_decision_signing_secret() -> Result<(), DecisionSigningError> {
    resolve_decision_signing_secret().map(drop)
}

pub fn sign_decision_spec(value: &Value) -> Result<String, DecisionSigningError> {
    let secret = resolve_decision_signing_secret()?;
    sign_decision_spec_with_secret(value, &secret)
}

pub fn verify_decision_spec(value: &Value, signature: &str) -> Result<bool, DecisionSigningError> {
    let secret = resolve_decision_signing_secret()?;
    verify_decision_spec_with_secret(value, signature, &secret)
}

pub fn sign_decision_spec_with_secret(
    value: &Value,
    secret: &str,
) -> Result<String, DecisionSigningError> {
    let secret = validated_secret(secret)?;
    let payload = format!("{DECISION_SIGNING_VERSION}:{}", canonical::canonical(value));
    let digest = crate::hmac_sha256(secret.as_bytes(), payload.as_bytes());
    Ok(format!("{DECISION_SIGNING_VERSION}.{digest}"))
}

pub fn verify_decision_spec_with_secret(
    value: &Value,
    signature: &str,
    secret: &str,
) -> Result<bool, DecisionSigningError> {
    let secret = validated_secret(secret)?;
    let prefix = format!("{DECISION_SIGNING_VERSION}.");
    let Some(encoded_tag) = signature.strip_prefix(&prefix) else {
        return Ok(false);
    };
    if encoded_tag.len() != 64
        || !encoded_tag
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return Ok(false);
    }

    let mut actual_tag = [0_u8; 32];
    if hex::decode_to_slice(encoded_tag, &mut actual_tag).is_err() {
        return Ok(false);
    }

    let payload = format!("{DECISION_SIGNING_VERSION}:{}", canonical::canonical(value));
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key");
    mac.update(payload.as_bytes());
    Ok(mac.verify_slice(&actual_tag).is_ok())
}

fn validated_secret(secret: &str) -> Result<&str, DecisionSigningError> {
    let trimmed = secret.trim();
    if javascript_string_length(trimmed) < MIN_DECISION_SIGNING_SECRET_LENGTH {
        return Err(DecisionSigningError::ExplicitSecretTooShort);
    }
    Ok(trimmed)
}

pub(crate) fn javascript_string_length(value: &str) -> usize {
    value.encode_utf16().count()
}

#[cfg(test)]
mod tests;
