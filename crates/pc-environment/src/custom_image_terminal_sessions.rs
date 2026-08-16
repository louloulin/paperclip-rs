//! Custom-image terminal session store + connection registry.
//!
//! Mirrors Node `server/src/services/environment-custom-image-terminal-sessions.ts` (353 lines) 1:1:
//!
//! - `parse_custom_image_setup_ssh_command`
//! - `validate_custom_image_setup_ssh_payload`
//! - `EnvironmentCustomImageTerminalSessionStore` (in-memory; singleton)
//! - `EnvironmentCustomImageTerminalConnectionRegistry` (in-memory; singleton)
//!
//! Re-uses `custom_image_setup_session_utils` for date coercion.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use chrono::{DateTime, Utc};
use rand::RngCore;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::custom_image_setup_session_utils::read_nullable_date;

// ============================================================================
// Constants
// ============================================================================

pub const DEFAULT_TERMINAL_SESSION_TOKEN_TTL_MS: i64 = 5 * 60 * 1000;
pub const TERMINAL_SESSION_TOKEN_BYTES: usize = 32;

// ============================================================================
// Types
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ParsedCustomImageSetupSshCommand {
    pub username: String,
    pub host: String,
    pub port: u16,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EnvironmentCustomImageTerminalPayloadValidationFailureCode {
    UnsupportedPayload,
    MissingCommand,
    UnsupportedCommand,
    InvalidExpiry,
    ExpiredPayload,
}

impl EnvironmentCustomImageTerminalPayloadValidationFailureCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UnsupportedPayload => "unsupported_payload",
            Self::MissingCommand => "missing_command",
            Self::UnsupportedCommand => "unsupported_command",
            Self::InvalidExpiry => "invalid_expiry",
            Self::ExpiredPayload => "expired_payload",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "ok", rename_all = "lowercase")]
pub enum EnvironmentCustomImageTerminalPayloadValidationResult {
    True {
        ssh: ParsedCustomImageSetupSshCommand,
        #[serde(rename = "connectionExpiresAt")]
        connection_expires_at: Option<DateTime<Utc>>,
    },
    False {
        status: u16,
        code: EnvironmentCustomImageTerminalPayloadValidationFailureCode,
        message: String,
    },
}

impl EnvironmentCustomImageTerminalPayloadValidationResult {
    pub fn is_ok(&self) -> bool {
        matches!(self, Self::True { .. })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentCustomImageTerminalSessionRecord {
    pub id: String,
    #[serde(rename = "setupSessionId")]
    pub setup_session_id: String,
    #[serde(rename = "companyId")]
    pub company_id: String,
    #[serde(rename = "environmentId")]
    pub environment_id: String,
    pub provider: String,
    #[serde(rename = "connectionType")]
    pub connection_type: String, // always "ssh"
    pub ssh: ParsedCustomImageSetupSshCommand,
    #[serde(rename = "hostKeySha256")]
    pub host_key_sha256: Option<String>,
    #[serde(rename = "createdAt")]
    pub created_at: DateTime<Utc>,
    #[serde(rename = "connectExpiresAt")]
    pub connect_expires_at: DateTime<Utc>,
    #[serde(rename = "sessionExpiresAt")]
    pub session_expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct MintedEnvironmentCustomImageTerminalSession {
    pub token: String,
    pub session: EnvironmentCustomImageTerminalSessionRecord,
}

#[derive(Debug, Clone)]
struct StoredEnvironmentCustomImageTerminalSession {
    token_hash: String,
    session: EnvironmentCustomImageTerminalSessionRecord,
}

pub type EnvironmentCustomImageTerminalConnectionClose = Box<dyn Fn(String) + Send + Sync>;

// ============================================================================
// SSH command parsing
// ============================================================================

fn parse_port(value: &str) -> Option<u16> {
    if value.len() > 5 {
        return None;
    }
    if !value.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    value.parse::<u16>().ok().filter(|p| *p >= 1 && *p <= 65535)
}

fn parse_destination(value: &str) -> Option<(String, String)> {
    if value.starts_with('-') {
        return None;
    }
    let mut parts = value.splitn(2, '@');
    let username = parts.next()?;
    let host = parts.next()?;
    if username.is_empty() || host.is_empty() {
        return None;
    }
    if username.contains(|c: char| c.is_whitespace() || c == '@' || c == '/') {
        return None;
    }
    if host.contains(|c: char| c.is_whitespace() || c == '@' || c == '/' || c == ':') {
        return None;
    }
    Some((username.to_string(), host.to_string()))
}

pub fn parse_custom_image_setup_ssh_command(
    command: &str,
) -> Option<ParsedCustomImageSetupSshCommand> {
    let tokens: Vec<&str> = command.trim().split_whitespace().collect();
    if tokens.first() != Some(&"ssh") {
        return None;
    }
    if tokens.len() == 2 {
        let (u, h) = parse_destination(tokens[1])?;
        return Some(ParsedCustomImageSetupSshCommand {
            username: u,
            host: h,
            port: 22,
        });
    }
    if tokens.len() != 4 {
        return None;
    }
    if tokens[1] == "-p" {
        let port = parse_port(tokens[2])?;
        let (u, h) = parse_destination(tokens[3])?;
        return Some(ParsedCustomImageSetupSshCommand {
            username: u,
            host: h,
            port,
        });
    }
    if tokens[2] == "-p" {
        let (u, h) = parse_destination(tokens[1])?;
        let port = parse_port(tokens[3])?;
        return Some(ParsedCustomImageSetupSshCommand {
            username: u,
            host: h,
            port,
        });
    }
    None
}

// ============================================================================
// Payload validation
// ============================================================================

fn read_connection_payload(value: &serde_json::Value) -> Option<&serde_json::Map<String, serde_json::Value>> {
    value.as_object()
}

pub fn validate_custom_image_setup_ssh_payload(
    payload: &serde_json::Value,
    now: DateTime<Utc>,
) -> EnvironmentCustomImageTerminalPayloadValidationResult {
    let record = match read_connection_payload(payload) {
        Some(r) => r,
        None => {
            return EnvironmentCustomImageTerminalPayloadValidationResult::False {
                status: 422,
                code: EnvironmentCustomImageTerminalPayloadValidationFailureCode::UnsupportedPayload,
                message: "Setup session terminal connections require an SSH connection payload.".to_string(),
            };
        }
    };
    if record.get("type").and_then(|v| v.as_str()) != Some("ssh") {
        return EnvironmentCustomImageTerminalPayloadValidationResult::False {
            status: 422,
            code: EnvironmentCustomImageTerminalPayloadValidationFailureCode::UnsupportedPayload,
            message: "Setup session terminal connections require an SSH connection payload.".to_string(),
        };
    }
    let command = record.get("command").and_then(|v| v.as_str()).unwrap_or("").trim();
    if command.is_empty() {
        return EnvironmentCustomImageTerminalPayloadValidationResult::False {
            status: 422,
            code: EnvironmentCustomImageTerminalPayloadValidationFailureCode::MissingCommand,
            message: "Setup session SSH payload is missing a supported command.".to_string(),
        };
    }
    let ssh = match parse_custom_image_setup_ssh_command(command) {
        Some(s) => s,
        None => {
            return EnvironmentCustomImageTerminalPayloadValidationResult::False {
                status: 422,
                code: EnvironmentCustomImageTerminalPayloadValidationFailureCode::UnsupportedCommand,
                message: "Setup session SSH payload uses an unsupported command shape.".to_string(),
            };
        }
    };
    let connection_expires_at = read_nullable_date(record.get("expiresAt"));
    if record.contains_key("expiresAt") && record.get("expiresAt") != Some(&serde_json::Value::Null) && connection_expires_at.is_none() {
        return EnvironmentCustomImageTerminalPayloadValidationResult::False {
            status: 422,
            code: EnvironmentCustomImageTerminalPayloadValidationFailureCode::InvalidExpiry,
            message: "Setup session SSH payload has an invalid expiry.".to_string(),
        };
    }
    if let Some(exp) = connection_expires_at {
        if exp <= now {
            return EnvironmentCustomImageTerminalPayloadValidationResult::False {
                status: 409,
                code: EnvironmentCustomImageTerminalPayloadValidationFailureCode::ExpiredPayload,
                message: "Setup session SSH connection payload has expired.".to_string(),
            };
        }
    }
    EnvironmentCustomImageTerminalPayloadValidationResult::True {
        ssh,
        connection_expires_at,
    }
}

// ============================================================================
// Internal helpers
// ============================================================================

fn hash_terminal_session_token(token: &str) -> String {
    let digest = Sha256::digest(token.as_bytes());
    let mut hex = String::with_capacity(digest.len() * 2);
    for b in digest {
        use std::fmt::Write as _;
        let _ = write!(&mut hex, "{:02x}", b);
    }
    hex
}

fn min_date(dates: &[DateTime<Utc>]) -> DateTime<Utc> {
    let mut iter = dates.iter();
    let first = iter.next().expect("at least one date");
    let mut best = *first;
    for d in iter {
        if *d < best {
            best = *d;
        }
    }
    best
}

fn to_valid_future_date(
    value: Option<&serde_json::Value>,
    now: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    crate::custom_image_setup_session_utils::read_future_date(value, now)
}

fn normalize_host_key_sha256(value: &serde_json::Value) -> Option<String> {
    let s = value.as_str()?;
    let trimmed = s.trim();
    if trimmed.is_empty() || trimmed.len() > 256 {
        None
    } else {
        Some(trimmed.to_string())
    }
}

// ============================================================================
// Session Store
// ============================================================================

pub struct EnvironmentCustomImageTerminalSessionStore {
    sessions_by_id: Mutex<HashMap<String, StoredEnvironmentCustomImageTerminalSession>>,
}

impl EnvironmentCustomImageTerminalSessionStore {
    pub fn new() -> Self {
        Self {
            sessions_by_id: Mutex::new(HashMap::new()),
        }
    }

    pub fn create(
        &self,
        input: CreateTerminalSessionInput,
    ) -> Result<MintedEnvironmentCustomImageTerminalSession, String> {
        let now = input.now.unwrap_or_else(Utc::now);
        self.cleanup_expired_at(now);

        let setup_expires_at = to_valid_future_date(input.setup_expires_at.as_ref(), now)
            .ok_or_else(|| "Terminal sessions require a future setup session expiry.".to_string())?;
        let connect_expires_at = to_valid_future_date(input.connection_expires_at.as_ref(), now);
        let default_window = now + chrono::Duration::milliseconds(DEFAULT_TERMINAL_SESSION_TOKEN_TTL_MS);
        let mut candidates = vec![default_window, setup_expires_at];
        if let Some(c) = connect_expires_at {
            candidates.push(c);
        }
        let connect_expires_at = min_date(&candidates);

        let mut bytes = vec![0u8; TERMINAL_SESSION_TOKEN_BYTES];
        rand::thread_rng().fill_bytes(&mut bytes);
        let token = base64_url_encode(&bytes);
        let id = Uuid::new_v4().to_string();
        let session = EnvironmentCustomImageTerminalSessionRecord {
            id: id.clone(),
            setup_session_id: input.setup_session_id,
            company_id: input.company_id,
            environment_id: input.environment_id,
            provider: input.provider,
            connection_type: "ssh".to_string(),
            ssh: input.ssh,
            host_key_sha256: None,
            created_at: now,
            connect_expires_at,
            session_expires_at: setup_expires_at,
        };
        let token_hash = hash_terminal_session_token(&token);
        self.sessions_by_id
            .lock()
            .unwrap()
            .insert(id.clone(), StoredEnvironmentCustomImageTerminalSession { token_hash, session: session.clone() });
        Ok(MintedEnvironmentCustomImageTerminalSession { token, session })
    }

    pub fn get(&self, id: &str, token: &str, now: DateTime<Utc>) -> Option<EnvironmentCustomImageTerminalSessionRecord> {
        if id.is_empty() || token.is_empty() {
            return None;
        }
        let mut guard = self.sessions_by_id.lock().unwrap();
        let stored = guard.get(id)?.clone();
        if stored.token_hash != hash_terminal_session_token(token) {
            return None;
        }
        if stored.session.connect_expires_at <= now {
            guard.remove(id);
            return None;
        }
        Some(stored.session)
    }

    pub fn get_by_id(&self, id: &str, now: DateTime<Utc>) -> Option<EnvironmentCustomImageTerminalSessionRecord> {
        if id.is_empty() {
            return None;
        }
        let mut guard = self.sessions_by_id.lock().unwrap();
        let stored = guard.get(id)?.clone();
        if stored.session.session_expires_at <= now {
            guard.remove(id);
            return None;
        }
        Some(stored.session)
    }

    pub fn verify_or_pin_host_key(&self, id: &str, host_key_sha256: &str, now: DateTime<Utc>) -> bool {
        let normalized = match normalize_host_key_sha256(&serde_json::Value::String(host_key_sha256.to_string())) {
            Some(n) => n,
            None => return false,
        };
        if id.is_empty() {
            return false;
        }
        let mut guard = self.sessions_by_id.lock().unwrap();
        let stored = match guard.get_mut(id) {
            Some(s) => s,
            None => return false,
        };
        if stored.session.session_expires_at <= now {
            guard.remove(id);
            return false;
        }
        if stored.session.host_key_sha256.is_none() {
            stored.session.host_key_sha256 = Some(normalized);
            return true;
        }
        stored.session.host_key_sha256.as_deref() == Some(normalized.as_str())
    }

    pub fn delete(&self, id: &str) -> bool {
        if id.is_empty() {
            return false;
        }
        self.sessions_by_id.lock().unwrap().remove(id).is_some()
    }

    pub fn delete_by_setup_session_id(&self, setup_session_id: &str) -> usize {
        if setup_session_id.is_empty() {
            return 0;
        }
        let mut guard = self.sessions_by_id.lock().unwrap();
        let mut removed = 0;
        let to_remove: Vec<String> = guard
            .iter()
            .filter_map(|(id, s)| {
                if s.session.setup_session_id == setup_session_id {
                    Some(id.clone())
                } else {
                    None
                }
            })
            .collect();
        for id in to_remove {
            if guard.remove(&id).is_some() {
                removed += 1;
            }
        }
        removed
    }

    pub fn cleanup_expired_at(&self, now: DateTime<Utc>) -> usize {
        let mut guard = self.sessions_by_id.lock().unwrap();
        let to_remove: Vec<String> = guard
            .iter()
            .filter_map(|(id, s)| {
                if s.session.session_expires_at <= now {
                    Some(id.clone())
                } else {
                    None
                }
            })
            .collect();
        let mut removed = 0;
        for id in to_remove {
            if guard.remove(&id).is_some() {
                removed += 1;
            }
        }
        removed
    }

    pub fn clear(&self) {
        self.sessions_by_id.lock().unwrap().clear();
    }
}

impl Default for EnvironmentCustomImageTerminalSessionStore {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug)]
pub struct CreateTerminalSessionInput {
    pub setup_session_id: String,
    pub company_id: String,
    pub environment_id: String,
    pub provider: String,
    pub ssh: ParsedCustomImageSetupSshCommand,
    pub setup_expires_at: Option<serde_json::Value>,
    pub connection_expires_at: Option<serde_json::Value>,
    pub now: Option<DateTime<Utc>>,
}

pub static ENVIRONMENT_CUSTOM_IMAGE_TERMINAL_SESSION_STORE: std::sync::LazyLock<EnvironmentCustomImageTerminalSessionStore> =
    std::sync::LazyLock::new(EnvironmentCustomImageTerminalSessionStore::new);

// ============================================================================
// Connection Registry
// ============================================================================

pub struct EnvironmentCustomImageTerminalConnectionRegistry {
    inner: std::sync::Arc<RegistryInner>,
}

struct RegistryInner {
    connections_by_setup_session_id: Mutex<HashMap<String, HashSet<usize>>>,
    closures: Mutex<Vec<EnvironmentCustomImageTerminalConnectionClose>>,
}

impl EnvironmentCustomImageTerminalConnectionRegistry {
    pub fn new() -> Self {
        Self {
            inner: std::sync::Arc::new(RegistryInner {
                connections_by_setup_session_id: Mutex::new(HashMap::new()),
                closures: Mutex::new(Vec::new()),
            }),
        }
    }

    pub fn add(self: std::sync::Arc<Self>, setup_session_id: &str, close: EnvironmentCustomImageTerminalConnectionClose) -> impl FnOnce() + Send + 'static {
        let inner = self.inner.clone();
        let id = {
            let mut closures = inner.closures.lock().unwrap();
            closures.push(close);
            closures.len() - 1
        };
        let key = setup_session_id.to_string();
        {
            let mut map = inner.connections_by_setup_session_id.lock().unwrap();
            let entry = map.entry(key.clone()).or_insert_with(HashSet::new);
            entry.insert(id);
        }
        move || {
            let mut map = inner.connections_by_setup_session_id.lock().unwrap();
            if let Some(set) = map.get_mut(&key) {
                set.remove(&id);
                if set.is_empty() {
                    map.remove(&key);
                }
            }
        }
    }

    pub fn close_by_setup_session_id(&self, setup_session_id: &str, reason: &str) -> usize {
        let ids = {
            let mut map = self.inner.connections_by_setup_session_id.lock().unwrap();
            map.remove(setup_session_id).unwrap_or_default()
        };
        if ids.is_empty() {
            return 0;
        }
        let mut closures = self.inner.closures.lock().unwrap();
        let mut closed = 0;
        for id in ids {
            if let Some(c) = closures.get(id) {
                c(reason.to_string());
                closed += 1;
            }
        }
        closed
    }

    pub fn close_all(&self, reason: &str) -> usize {
        let setup_session_ids: Vec<String> = {
            let map = self.inner.connections_by_setup_session_id.lock().unwrap();
            map.keys().cloned().collect()
        };
        let mut total = 0;
        for sid in setup_session_ids {
            total += self.close_by_setup_session_id(&sid, reason);
        }
        total
    }

    pub fn clear(&self) {
        self.inner.connections_by_setup_session_id.lock().unwrap().clear();
        self.inner.closures.lock().unwrap().clear();
    }
}

impl Default for EnvironmentCustomImageTerminalConnectionRegistry {
    fn default() -> Self {
        Self::new()
    }
}

pub static ENVIRONMENT_CUSTOM_IMAGE_TERMINAL_CONNECTION_REGISTRY: std::sync::LazyLock<EnvironmentCustomImageTerminalConnectionRegistry> =
    std::sync::LazyLock::new(EnvironmentCustomImageTerminalConnectionRegistry::new);

fn base64_url_encode(bytes: &[u8]) -> String {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
    URL_SAFE_NO_PAD.encode(bytes)
}

use serde::{Deserialize, Serialize};
