#![forbid(unsafe_code)]

//! Username / home-directory redaction for log lines + JSON values.
//!
//! R526: Direct port of `paperclip/server/src/log-redaction.ts`.
//!
//! 设计原则:
//! - **所有 `pub fn` 都是纯函数** (无 IO, 无副作用, 无 env 读取 — caller 显式传 Options)
//! - 接受 `&str` / `&serde_json::Value` 输入, 返回 owned String / Value
//! - 不引入 `std::env` 读取 — 测试不依赖环境变量, 100% deterministic
//! - 不抛异常 — 任何输入都返回合理结果 (空字符串 / unknown)
//! - 跨平台: 同时处理 `/` (Unix) 和 `\\` (Windows) path separator
//!
//! 与 Node 上游 [`log-redaction.ts`] 的差异:
//! - Rust 端不缓存 env candidates — caller 控制 lifetime, 测试更确定
//! - Rust 端 `redact_value` 接受 `&serde_json::Value` 而非任意 JS 对象 (类型安全)
//! - Rust 端 `mask_user_name` 返回 owned `String` 而非 Node 模板字符串

pub mod mask;
pub mod path;
pub mod text;
pub mod value;

/// Default token substituted for the current user / home directory
/// in log lines. Matches Node upstream `CURRENT_USER_REDACTION_TOKEN`.
pub const CURRENT_USER_REDACTION_TOKEN: &str = "*";

/// Per-call configuration for redaction.
///
/// All fields are optional. When `user_names` / `home_dirs` are `None`,
/// callers must pre-populate them via [`Options::with_default_candidates`]
/// (which reads from process env at construction time).
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// When `false`, [`text::redact_current_user_text`] returns the input
    /// unchanged (mirrors Node `opts?.enabled === false`).
    pub enabled: bool,
    /// Replacement token. Defaults to [`CURRENT_USER_REDACTION_TOKEN`].
    pub replacement: String,
    /// Candidate usernames to redact (longest first, deterministic order).
    pub user_names: Vec<String>,
    /// Candidate home directories to redact (longest first).
    pub home_dirs: Vec<String>,
}

impl Options {
    /// Build an `Options` populated with default candidates from `env`.
    ///
    /// The Node upstream caches the result in a module-level singleton; the
    /// Rust version makes this explicit at the call site (no hidden global
    /// state, no test flakiness from leaked env vars between tests).
    pub fn with_default_candidates(env: &dyn Env) -> Self {
        let user_names = default_user_names(env);
        let home_dirs = default_home_dirs(env, &user_names);
        Self {
            enabled: true,
            replacement: CURRENT_USER_REDACTION_TOKEN.to_string(),
            user_names,
            home_dirs,
        }
    }
}

/// Abstraction over process env so callers (and tests) can inject env
/// values without mutating `std::env`.
pub trait Env {
    fn var(&self, key: &str) -> Option<String>;
}

/// Production [`Env`] backed by `std::env`.
#[derive(Debug, Default, Clone, Copy)]
pub struct StdEnv;

impl Env for StdEnv {
    fn var(&self, key: &str) -> Option<String> {
        std::env::var(key).ok()
    }
}

/// Look up the current user's username from common env vars.
pub fn default_user_names(env: &dyn Env) -> Vec<String> {
    let mut candidates: Vec<Option<String>> = Vec::new();
    for key in ["USER", "LOGNAME", "USERNAME"] {
        candidates.push(env.var(key));
    }
    unique_non_empty(candidates.into_iter())
}

/// Look up the current user's home directory from common env vars.
/// Always includes `/Users/{user}`, `/home/{user}`, and `C:\\Users\\{user}`
/// per-user candidates.
pub fn default_home_dirs(env: &dyn Env, user_names: &[String]) -> Vec<String> {
    let mut candidates: Vec<Option<String>> = Vec::new();
    for key in ["HOME", "USERPROFILE"] {
        candidates.push(env.var(key));
    }
    for user in user_names {
        candidates.push(Some(format!("/Users/{user}")));
        candidates.push(Some(format!("/home/{user}")));
        candidates.push(Some(format!("C:\\Users\\{user}")));
    }
    unique_non_empty(candidates.into_iter())
}

fn unique_non_empty<I>(values: I) -> Vec<String>
where
    I: IntoIterator<Item = Option<String>>,
{
    let mut set = std::collections::BTreeSet::new();
    for v in values {
        if let Some(s) = v {
            let trimmed = s.trim();
            if !trimmed.is_empty() {
                set.insert(trimmed.to_string());
            }
        }
    }
    set.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Mock env for tests.
    struct MockEnv(Vec<(String, String)>);
    impl Env for MockEnv {
        fn var(&self, key: &str) -> Option<String> {
            self.0
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.clone())
        }
    }

    #[test]
    fn r526_unique_non_empty_dedupes_and_trims() {
        let v = vec![
            Some("a".into()),
            Some(" a ".into()),
            Some("".into()),
            None,
            Some("b".into()),
        ];
        let out = unique_non_empty(v.into_iter());
        assert_eq!(out, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn r526_default_user_names_returns_env_var() {
        let env = MockEnv(vec![("USER".into(), "alice".into())]);
        let names = default_user_names(&env);
        assert!(names.contains(&"alice".to_string()));
    }

    #[test]
    fn r526_default_home_dirs_includes_user_paths() {
        let env = MockEnv(vec![("HOME".into(), "/home/alice".into())]);
        let dirs = default_home_dirs(&env, &["alice".into()]);
        assert!(dirs.contains(&"/home/alice".to_string()));
        assert!(dirs.contains(&"/Users/alice".to_string()));
        assert!(dirs.contains(&"C:\\Users\\alice".to_string()));
    }

    #[test]
    fn r526_options_with_default_candidates_uses_env() {
        let env = MockEnv(vec![
            ("USER".into(), "alice".into()),
            ("HOME".into(), "/home/alice".into()),
        ]);
        let opts = Options::with_default_candidates(&env);
        assert!(opts.enabled);
        assert_eq!(opts.replacement, "*");
        assert!(opts.user_names.contains(&"alice".to_string()));
        assert!(opts.home_dirs.contains(&"/home/alice".to_string()));
    }

    #[test]
    fn r526_options_disabled_passes_through() {
        let env = MockEnv(vec![("USER".into(), "alice".into())]);
        let mut opts = Options::with_default_candidates(&env);
        opts.enabled = false;
        assert_eq!(
            crate::text::redact_current_user_text("hello alice", &opts),
            "hello alice"
        );
    }
}
