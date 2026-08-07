//! `pc-acpx` paperclip Claude settings writer — mirrors Node
//! `writePaperclipClaudeSettings`. The Claude Code SDK used by
//! `claude-agent-acp` honors `settingSources: ["user", "project", "local"]`,
//! so we materialize a per-worktree `.claude/settings.local.json` that
//! grants the Paperclip bridge commands access without forcing the agent
//! through a permission prompt for every Bash call.
//!
//! The writer preserves the user's existing allow / additionalDirectories
//! entries, prepends Paperclip-specific entries, and force-overrides a
//! `defaultMode: "dontAsk"` so the bridge commands can run unprompted.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::AcpxError;
use crate::fs_ops::{ensure_parent_dir, write_file_atomically, WriteFileAtomicallyInput};
use crate::session_compat::unique_sorted;

const SETTINGS_RELATIVE_PATH: &str = ".claude/settings.local.json";

/// Input to [`paperclip_claude_settings_write_with`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaudeSettingsWriteInput {
    /// The Paperclip instance root. Mirrors the result of
    /// `default_paperclip_instance_dir()`. We re-derive the company root
    /// from this rather than re-resolving the env, so the call is fully
    /// deterministic from the inputs.
    pub instance_root: String,
    /// The agent's working directory (`sessionCwd`). The settings file is
    /// written inside this directory.
    pub cwd: String,
    /// The agent's per-run state directory (granted as an additional dir).
    pub state_dir: String,
    /// The agent's managed home directory (granted as an additional dir).
    pub agent_home: String,
    /// The owning company id (used to compute the company root grant).
    pub company_id: String,
}

/// Result of [`paperclip_claude_settings_write_with`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaperclipClaudeSettingsResult {
    /// Absolute path to the settings file we wrote (or overwrote).
    pub file_path: String,
    /// The merged `permissions.allow` list after the writer ran.
    pub allow: Vec<String>,
    /// The merged `permissions.additionalDirectories` list after the writer
    /// ran.
    pub additional_directories: Vec<String>,
    /// The resolved `permissions.defaultMode` value (`"default"` when the
    /// existing `dontAsk` was overridden, otherwise the existing value).
    pub default_mode: String,
    /// `true` when the existing settings had `defaultMode: "dontAsk"` and
    /// we force-overrode it to `"default"`. Callers may use this flag to
    /// surface a one-shot warning to the user.
    pub overrode_dont_ask: bool,
}

/// Write the per-worktree `.claude/settings.local.json` for the run. The
/// call is idempotent: re-running merges rather than duplicates. Mirrors
/// Node `writePaperclipClaudeSettings`.
pub async fn paperclip_claude_settings_write_with(
    input: ClaudeSettingsWriteInput,
) -> Result<PaperclipClaudeSettingsResult, AcpxError> {
    let cwd = PathBuf::from(&input.cwd);
    let file_path = cwd.join(SETTINGS_RELATIVE_PATH);
    let instance_root = PathBuf::from(&input.instance_root);
    let company_root = instance_root.join("companies").join(&input.company_id);
    let paperclip_additional = unique_sorted(
        [
            Some(input.state_dir.clone()),
            Some(input.agent_home.clone()),
            Some(company_root.to_string_lossy().into_owned()),
        ]
        .into_iter(),
    );
    let paperclip_allow = unique_sorted(
        [
            Some("Bash(curl:*)".to_string()),
            Some("Bash(env:*)".to_string()),
            Some("Bash(env)".to_string()),
            Some(format!(
                "Bash({}/scripts/paperclip-issue-update.sh:*)",
                input.cwd
            )),
            Some(format!("Bash({}/scripts/paperclip:*)", input.cwd)),
        ]
        .into_iter(),
    );

    let mut existing: serde_json::Map<String, serde_json::Value> = serde_json::Map::new();
    if let Ok(raw) = std::fs::read_to_string(&file_path) {
        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&raw) {
            if let serde_json::Value::Object(map) = parsed {
                existing = map;
            }
        }
    }

    let existing_perms = existing
        .get("permissions")
        .cloned()
        .unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new()));
    let existing_perms_map = match existing_perms {
        serde_json::Value::Object(map) => map,
        _ => serde_json::Map::new(),
    };
    let existing_allow: Vec<String> = match existing_perms_map.get("allow") {
        Some(serde_json::Value::Array(items)) => items
            .iter()
            .filter_map(|value| value.as_str().map(str::to_string))
            .collect(),
        _ => Vec::new(),
    };
    let existing_additional: Vec<String> = match existing_perms_map.get("additionalDirectories") {
        Some(serde_json::Value::Array(items)) => items
            .iter()
            .filter_map(|value| value.as_str().map(str::to_string))
            .collect(),
        _ => Vec::new(),
    };
    let merged_allow = unique_sorted(
        existing_allow
            .into_iter()
            .map(Some)
            .chain(paperclip_allow.into_iter().map(Some)),
    );
    let merged_additional = unique_sorted(
        existing_additional
            .into_iter()
            .map(Some)
            .chain(paperclip_additional.into_iter().map(Some)),
    );
    let existing_default_mode = existing_perms_map
        .get("defaultMode")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    let (default_mode, overrode_dont_ask) =
        if existing_default_mode.is_empty() || existing_default_mode == "dontAsk" {
            let overrode = existing_default_mode == "dontAsk";
            ("default".to_string(), overrode)
        } else {
            (existing_default_mode.clone(), false)
        };

    let mut next_perms = existing_perms_map;
    next_perms.insert(
        "allow".to_string(),
        serde_json::Value::Array(
            merged_allow
                .iter()
                .map(|value| serde_json::Value::String(value.clone()))
                .collect(),
        ),
    );
    next_perms.insert(
        "additionalDirectories".to_string(),
        serde_json::Value::Array(
            merged_additional
                .iter()
                .map(|value| serde_json::Value::String(value.clone()))
                .collect(),
        ),
    );
    next_perms.insert(
        "defaultMode".to_string(),
        serde_json::Value::String(default_mode.clone()),
    );
    existing.insert(
        "permissions".to_string(),
        serde_json::Value::Object(next_perms),
    );

    let body =
        serde_json::to_string_pretty(&serde_json::Value::Object(existing)).map_err(|error| {
            AcpxError::Json {
                context: "paperclip_claude_settings_write_with".to_string(),
                error,
            }
        })?;
    ensure_parent_dir(&file_path).await?;
    write_file_atomically(WriteFileAtomicallyInput::new(
        &file_path,
        format!("{body}\n"),
        0o600,
    ))
    .await?;

    Ok(PaperclipClaudeSettingsResult {
        file_path: file_path.to_string_lossy().into_owned(),
        allow: merged_allow,
        additional_directories: merged_additional,
        default_mode,
        overrode_dont_ask,
    })
}

/// Walk `local_path` and produce a short, deterministic content signature.
///
/// Symlinks record their target; directories are descended (skipping the
/// configured skip set). Any I/O error downgrades the signature to an
/// `unreadable:...` string so the caller can distinguish "changed" from
/// "unreadable" via the prefix. Mirrors Node
/// `referencedSourceContentSignature`.
pub fn referenced_source_content_signature(local_path: &Path) -> Result<String, AcpxError> {
    const SKIP_DIRS: &[&str] = &["node_modules", ".git", "target", "dist", ".next"];
    let mut hasher = Sha256::new();
    let outcome = walk_for_signature(local_path, Path::new(""), &mut hasher, SKIP_DIRS);
    match outcome {
        SignatureWalk::Ok => {
            let digest = hasher.finalize();
            let hex = format!("{:x}", digest);
            Ok(hex[..16].to_string())
        }
        SignatureWalk::Unreadable(reason) => Ok(format!("unreadable:{reason}")),
    }
}

enum SignatureWalk {
    Ok,
    Unreadable(String),
}

fn walk_for_signature(
    root: &Path,
    relative: &Path,
    hasher: &mut Sha256,
    skip_dirs: &[&str],
) -> SignatureWalk {
    let current = if relative.as_os_str().is_empty() {
        root.to_path_buf()
    } else {
        root.join(relative)
    };
    let dirents = match std::fs::read_dir(&current) {
        Ok(dirents) => dirents,
        Err(error) => return SignatureWalk::Unreadable(error.to_string()),
    };
    let mut names: Vec<String> = Vec::new();
    for entry in dirents {
        match entry {
            Ok(entry) => names.push(entry.file_name().to_string_lossy().into_owned()),
            Err(error) => return SignatureWalk::Unreadable(error.to_string()),
        }
    }
    names.sort();
    for name in names {
        let next_relative: PathBuf = if relative.as_os_str().is_empty() {
            PathBuf::from(&name)
        } else {
            relative.join(&name)
        };
        let absolute = root.join(&next_relative);
        let metadata = match std::fs::symlink_metadata(&absolute) {
            Ok(metadata) => metadata,
            Err(error) => return SignatureWalk::Unreadable(error.to_string()),
        };
        let file_type = metadata.file_type();
        if file_type.is_dir() {
            if skip_dirs.iter().any(|skip| skip == &name.as_str()) {
                continue;
            }
            match walk_for_signature(root, &next_relative, hasher, skip_dirs) {
                SignatureWalk::Ok => continue,
                other => return other,
            }
        } else if file_type.is_symlink() {
            let target = match std::fs::read_link(&absolute) {
                Ok(target) => target.to_string_lossy().into_owned(),
                Err(error) => return SignatureWalk::Unreadable(error.to_string()),
            };
            hasher.update(format!("symlink:{}:{}\n", next_relative.display(), target).as_bytes());
        } else if file_type.is_file() {
            let size = metadata.len();
            hasher.update(format!("file:{}:{}\n", next_relative.display(), size).as_bytes());
            let bytes = match std::fs::read(&absolute) {
                Ok(bytes) => bytes,
                Err(error) => return SignatureWalk::Unreadable(error.to_string()),
            };
            hasher.update(&bytes);
            hasher.update(b"\n");
        } else {
            let mode = mode_of(&metadata);
            hasher.update(format!("other:{}:{mode}\n", next_relative.display()).as_bytes());
        }
    }
    SignatureWalk::Ok
}

#[cfg(unix)]
fn mode_of(metadata: &std::fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode()
}

#[cfg(not(unix))]
fn mode_of(_metadata: &std::fs::Metadata) -> u32 {
    0
}
