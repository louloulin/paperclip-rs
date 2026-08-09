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

/// 规范化 cwd 字符串：清理 `./` / `../`，保留绝对路径字面量。
///
/// Node 等价（pi-local `normalizeExecutionCwd` 与
/// claude-local `claudeSessionCwdMatchesExecutionTarget` 内的 `path.resolve`）。
///
/// 与 `Path::canonicalize` 不同：纯字符串处理，不做 fs 解析。
///
/// 实现要点：避免 `out.join("/")` 在根前缀处产生 `"//a/b"` 双斜杠，
/// 因此把根前缀视为"前缀标记"，最后再以 `format!("/{}", ...)` 拼回。
pub fn normalize_cwd(candidate: &str) -> String {
    let path = std::path::Path::new(candidate);
    let mut absolute = false;
    let mut segments: Vec<String> = Vec::new();
    for comp in path.components() {
        match comp {
            std::path::Component::Prefix(_) => {
                absolute = true;
                segments.push(comp.as_os_str().to_string_lossy().into_owned());
            }
            std::path::Component::RootDir => {
                absolute = true;
            }
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                let can_pop = segments.last().map(|last| last != "..").unwrap_or(false);
                if can_pop {
                    segments.pop();
                } else if !absolute {
                    segments.push("..".to_owned());
                }
                // absolute + 栈空/栈顶 `..` → 忽略（已经在根目录之上）
            }
            std::path::Component::Normal(part) => {
                segments.push(part.to_string_lossy().into_owned());
            }
        }
    }
    if segments.is_empty() {
        if absolute {
            "/".to_owned()
        } else {
            ".".to_owned()
        }
    } else if absolute {
        format!("/{}", segments.join("/"))
    } else {
        segments.join("/")
    }
}

/// 比较两个 cwd 是否指向同一逻辑路径（POSIX 大小写敏感）。
///
/// Node 等价：pi-local `executionCwdsMatch` / claude-local 内部比较。
pub fn cwds_match(saved: &str, current: &str) -> bool {
    normalize_cwd(saved) == normalize_cwd(current)
}

#[cfg(test)]
mod path_utils_tests {
    use super::*;

    #[test]
    fn normalize_cwd_绝对路径() {
        assert_eq!(normalize_cwd("/a/b/c"), "/a/b/c");
        assert_eq!(normalize_cwd("/a/./b"), "/a/b");
        assert_eq!(normalize_cwd("/a/b/../c"), "/a/c");
        assert_eq!(normalize_cwd("/a/b/c/.."), "/a/b");
    }

    #[test]
    fn normalize_cwd_相对路径() {
        assert_eq!(normalize_cwd("a/b"), "a/b");
        assert_eq!(normalize_cwd("./a/b"), "a/b");
        assert_eq!(normalize_cwd("a/./b"), "a/b");
        assert_eq!(normalize_cwd("a/b/../c"), "a/c");
    }

    #[test]
    fn normalize_cwd_根路径() {
        assert_eq!(normalize_cwd("/"), "/");
        assert_eq!(normalize_cwd("/."), "/");
        assert_eq!(normalize_cwd("/.."), "/");
    }

    #[test]
    fn normalize_cwd_空输入() {
        assert_eq!(normalize_cwd(""), ".");
    }

    #[test]
    fn cwds_match_基本() {
        assert!(cwds_match("/a/b", "/a/b"));
        assert!(!cwds_match("/a/b", "/a/c"));
        assert!(!cwds_match("/a/b", "/a/b/c"));
        assert!(!cwds_match("/a/B", "/a/b")); // 大小写敏感
    }

    #[test]
    fn cwds_match_规范化() {
        assert!(cwds_match("/a/./b", "/a/b"));
        assert!(cwds_match("/a/b/../c", "/a/c"));
    }
}
