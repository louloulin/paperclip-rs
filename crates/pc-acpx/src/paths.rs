//! `pc-acpx` paths — pure helpers that resolve the Paperclip instance root
//! and per-company / per-agent state directories. Mirrors the Node
//! `defaultPaperclipInstanceDir`, `defaultStateDir`, and
//! `resolveManagedCodexHomeDir` functions in `acpx-engine/execute.ts`.
//!
//! The pure resolver accepts a caller-provided env so tests can drive the
//! resolution deterministically without touching `std::env`. The
//! `*_with_env` wrappers read `std::env` for production use.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::error::AcpxError;

const DEFAULT_INSTANCE_ID: &str = "default";
const PAPERCLIP_HOME_ENV: &str = "PAPERCLIP_HOME";
const PAPERCLIP_INSTANCE_ID_ENV: &str = "PAPERCLIP_INSTANCE_ID";

/// Expand a leading `~` or `~/...` to the caller's home directory. Any other
/// value is returned verbatim. Mirrors Node `expandHomePrefix`.
pub fn expand_home_prefix(value: &str) -> PathBuf {
    if value == "~" {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home);
        }
        return PathBuf::from(value);
    }
    if let Some(stripped) = value.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(stripped);
        }
        return PathBuf::from(value);
    }
    PathBuf::from(value)
}

/// Resolve the Paperclip instance root. Mirrors Node
/// `resolvePaperclipInstanceRootForAdapter`:
///
/// 1. `home_dir` falls back to `PAPERCLIP_HOME` then `~/.paperclip`.
/// 2. `instance_id` falls back to `PAPERCLIP_INSTANCE_ID` then `default`.
/// 3. `instance_id` must match `[A-Za-z0-9_-]+` — anything else returns
///    `AcpxError::InvalidInstanceId`.
/// 4. The returned path is `<home>/instances/<instance_id>`, made absolute.
pub fn resolve_paperclip_instance_root(
    home_dir: Option<&str>,
    instance_id: Option<&str>,
    env: &HashMap<String, String>,
) -> Result<PathBuf, AcpxError> {
    let home_raw = home_dir
        .map(|s| s.trim())
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| {
            env.get(PAPERCLIP_HOME_ENV)
                .map(|s| s.trim())
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        })
        .unwrap_or_else(|| {
            PathBuf::from("~")
                .join(".paperclip")
                .to_string_lossy()
                .into_owned()
        });
    let home_resolved = expand_home_prefix(&home_raw);
    let home_absolute = if Path::new(&home_resolved).is_absolute() {
        home_resolved
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(home_resolved)
    };
    let resolved_instance = instance_id
        .map(|s| s.trim())
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| {
            env.get(PAPERCLIP_INSTANCE_ID_ENV)
                .map(|s| s.trim())
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        })
        .unwrap_or_else(|| DEFAULT_INSTANCE_ID.to_string());
    if !is_valid_instance_id(&resolved_instance) {
        return Err(AcpxError::InvalidInstanceId(resolved_instance));
    }
    Ok(home_absolute.join("instances").join(&resolved_instance))
}

/// Returns the instance root for the running process, reading
/// `PAPERCLIP_HOME` / `PAPERCLIP_INSTANCE_ID` from `std::env`. This is the
/// production equivalent of Node `defaultPaperclipInstanceDir`.
pub fn default_paperclip_instance_dir() -> PathBuf {
    let env = std::env::vars().collect::<HashMap<_, _>>();
    resolve_paperclip_instance_root(None, None, &env)
        .expect("std::env-derived instance id is always valid")
}

/// Resolve the per-company, per-agent state directory under
/// `<instance>/companies/<company>/acp-engine/agents/<agent>`. Mirrors Node
/// `defaultStateDir`.
pub fn default_state_dir(company_id: &str, agent_id: &str) -> PathBuf {
    default_paperclip_instance_dir()
        .join("companies")
        .join(company_id)
        .join("acp-engine")
        .join("agents")
        .join(agent_id)
}

/// Resolve the per-company managed Codex home directory under
/// `<instance>/companies/<company>/codex-home`. Mirrors Node
/// `resolveManagedCodexHomeDir`.
pub fn resolve_managed_codex_home_dir(company_id: &str) -> PathBuf {
    default_paperclip_instance_dir()
        .join("companies")
        .join(company_id)
        .join("codex-home")
}

fn is_valid_instance_id(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
}
