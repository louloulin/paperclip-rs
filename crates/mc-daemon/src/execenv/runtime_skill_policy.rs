//! provider 侧的「哪些 runtime skill 被关掉了」注入口（claude 的 settings、codex 的 config）。
//!
//! - **上游**：`execenv/runtime_skill_policy.go`（159 行）。
//! - **写者**：M6-9（本 slice）。
//!
//! ## claude：两个通道都要发
//!
//! Claude Code 的 `skillOverrides` 能把**个人/项目** skill 整个隐藏，但它**不看**插件 skill；
//! 所以每个键还要额外发一条 permission deny，那条才是插件 skill 的唯一强制通道（上游注释）。
//! `root == "plugin"` 的条目因此把 invocationName 换成**键**本身（插件 skill 的调用名带
//! `<plugin>:` 前缀），而 overrides 里不写它。
//!
//! ## codex：写的是 `[[skills.config]]` 块
//!
//! Codex 侧不是「开关文件」而是往 `config.toml` **追加**禁用块。这里**不需要 TOML
//! 解析器**（只是写文本），所以本 slice 能完整交付 —— 读 TOML 的那半（
//! [`crate::mcp::runtime`]）才是未接的那条。
//!
//! ## 与上游的差异（逐条登记在 `docs/32` §9.9）
//!
//! - `prepareClaudeSkillSettings` 里 `os.Remove(path)`（没有任何禁用项时清掉旧文件）本 slice
//!   照做；返回 `None` 表示「没有这个 sidecar」。
//! - `strconv.Quote` 的等价物是本文件里的 [`go_quote`]：覆盖路径里会出现的那些字符
//!   （`"` / `\` / `\n` / `\r` / `\t` / 其余控制字符），**不**覆盖 Go 的 `\u` 转义全集
//!   （非 ASCII 路径在两边都按 UTF-8 原样写入，行为一致，登记在案）。

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::execenv::sidecar::{
    create_dir_all, remove_file_if_present, sanitize_skill_name, SidecarError,
};
use crate::skill::{clean_slash_path, user_home, SkillForEnv};

/// claude 的运行时 skill settings 文件名（上游 `claudeRuntimeSkillSettingsFile`）。
pub const CLAUDE_RUNTIME_SKILL_SETTINGS_FILE: &str = "claude-runtime-skill-settings.json";

/// 一个 runtime 本地 skill 的引用（上游 `RuntimeSkillRefForEnv`）。
///
/// provider 与 runtime 已经由任务选定，所以这里只需要「发现根 + provider 原生键」。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeSkillRefForEnv {
    /// 发现根分类（`provider` / `universal` / `plugin`）。
    pub root: String,
    /// provider 原生键（斜杠相对路径）。
    pub key: String,
    /// 显示名（claude 的 overrides 用它，空则回落成键的最后一段）。
    pub name: String,
    /// 贡献插件 id（非插件根为空）。
    pub plugin: String,
}

/// 清理一个 runtime skill 键（上游 `cleanRuntimeSkillKey`）。
///
/// ⚠️ 判据与 [`crate::skill::normalize_local_skill_key`] **不同**：这里只拒 `.`、绝对路径、
/// 恰好是 `..`、以及 `../` 前缀 —— `..foo` 是**合法**的。两处口径不同是上游行为，照抄。
#[must_use]
pub fn clean_runtime_skill_key(key: &str) -> Option<String> {
    let cleaned = clean_slash_path(key.trim());
    if cleaned == "." || cleaned.starts_with('/') || cleaned == ".." || cleaned.starts_with("../") {
        return None;
    }
    Some(cleaned)
}

/// 这个显示名是否被一批 workspace skill 占用了（上游 `workspaceClaimsRuntimeSkill`）。
///
/// 比较双方都过 [`sanitize_skill_name`]：`"PR review"` 与 `"pr-review"` 是同一个盘上目录。
#[must_use]
pub fn workspace_claims_runtime_skill(name: &str, workspace_skills: &[SkillForEnv]) -> bool {
    let claim = sanitize_skill_name(name);
    workspace_skills
        .iter()
        .any(|skill| sanitize_skill_name(&skill.name) == claim)
}

/// 写 claude 的运行时 skill settings（上游 `prepareClaudeSkillSettings`）。
///
/// 返回写出的路径；没有任何禁用项（或全被 workspace skill 挡住）时**删掉旧文件**并返回
/// `None` —— 否则上一轮的禁用会留在这一轮的任务目录里。
pub fn prepare_claude_skill_settings(
    env_root: &Path,
    disabled: &[RuntimeSkillRefForEnv],
    workspace_skills: &[SkillForEnv],
) -> Result<Option<PathBuf>, SidecarError> {
    let path = env_root.join(CLAUDE_RUNTIME_SKILL_SETTINGS_FILE);
    if disabled.is_empty() {
        remove_file_if_present(&path)?;
        return Ok(None);
    }

    let mut overrides: BTreeMap<String, String> = BTreeMap::new();
    let mut deny: Vec<String> = Vec::with_capacity(disabled.len() * 2);
    let mut seen_deny: BTreeSet<String> = BTreeSet::new();
    let mut add_deny = |rule: String| {
        if seen_deny.insert(rule.clone()) {
            deny.push(rule);
        }
    };

    for skill in disabled {
        let Some(key) = clean_runtime_skill_key(&skill.key) else {
            continue;
        };
        let mut invocation_name = skill.name.trim().to_string();
        if invocation_name.is_empty() {
            invocation_name = key.rsplit('/').next().unwrap_or(key.as_str()).to_string();
        }
        if workspace_claims_runtime_skill(&invocation_name, workspace_skills) {
            continue;
        }
        if skill.root == crate::skill::ROOT_PLUGIN {
            invocation_name = key;
        } else {
            overrides.insert(invocation_name.clone(), "off".to_string());
        }
        add_deny(format!("Skill({invocation_name})"));
        add_deny(format!("Skill({invocation_name} *)"));
    }

    if overrides.is_empty() && deny.is_empty() {
        remove_file_if_present(&path)?;
        return Ok(None);
    }

    let payload = serde_json::json!({
        "skillOverrides": overrides,
        "permissions": { "deny": deny },
    });
    let data = serde_json::to_vec_pretty(&payload)
        .map_err(|err| SidecarError::Invalid(format!("marshal claude skill settings: {err}")))?;
    if let Some(parent) = path.parent() {
        create_dir_all(parent)?;
    }
    // 上游是 `os.WriteFile(path, data, 0o600)`：**允许覆盖**（这是 daemon 自己上一轮写的
    // 文件，且它按内容整体重写，不是用户资产）。
    if let Err(err) = std::fs::write(&path, &data) {
        return Err(SidecarError::Io {
            op: "write claude skill settings",
            path: path.clone(),
            source: err,
        });
    }
    Ok(Some(path))
}

/// 往 codex 的 `config.toml` 追加拿 `[[skills.config]]` 禁用块
/// （上游 `ensureCodexDisabledSkillsConfig`）。
///
/// `provider` 根的键映射到 `<codex_home>/skills/<key>/SKILL.md`，`universal` 根映射到
/// `<home>/.agents/skills/<key>/SKILL.md`；其余发现根**跳过**（插件 skill 不受这条控制）。
/// 同一路径只写一次。
pub fn ensure_codex_disabled_skills_config(
    config_path: &Path,
    codex_home: &Path,
    disabled: &[RuntimeSkillRefForEnv],
    workspace_skills: &[SkillForEnv],
) -> Result<(), SidecarError> {
    if disabled.is_empty() {
        return Ok(());
    }
    let mut home: Option<PathBuf> = None;
    let mut paths: Vec<String> = Vec::with_capacity(disabled.len());
    let mut seen: BTreeSet<String> = BTreeSet::new();

    for skill in disabled {
        let Some(key) = clean_runtime_skill_key(&skill.key) else {
            continue;
        };
        let skill_path = match skill.root.as_str() {
            crate::skill::ROOT_PROVIDER => {
                let first = key.split('/').next().unwrap_or_default();
                if workspace_claims_runtime_skill(first, workspace_skills) {
                    continue;
                }
                slash_path(&codex_home.join("skills").join(&key).join("SKILL.md"))
            }
            crate::skill::ROOT_UNIVERSAL => {
                if home.is_none() {
                    home = Some(user_home().map_err(|_| {
                        SidecarError::Invalid(
                            "resolve user home for disabled Codex skills".to_string(),
                        )
                    })?);
                }
                let base = home.clone().unwrap_or_default();
                slash_path(
                    &base
                        .join(".agents")
                        .join("skills")
                        .join(&key)
                        .join("SKILL.md"),
                )
            }
            _ => continue,
        };
        if !seen.insert(skill_path.clone()) {
            continue;
        }
        paths.push(skill_path);
    }
    if paths.is_empty() {
        return Ok(());
    }

    if let Some(parent) = config_path.parent() {
        create_dir_all(parent)?;
    }
    let mut body = String::new();
    for path in &paths {
        // 与上游逐字：先一个空行、块头、`path = "<quoted>"`、`enabled = false`。
        body.push_str("\n[[skills.config]]\npath = ");
        body.push_str(&go_quote(path));
        body.push_str("\nenabled = false\n");
    }
    use std::io::Write as _;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(config_path)
        .map_err(|err| SidecarError::Io {
            op: "append codex skills config",
            path: config_path.to_path_buf(),
            source: err,
        })?;
    file.write_all(body.as_bytes())
        .map_err(|err| SidecarError::Io {
            op: "append codex skills config",
            path: config_path.to_path_buf(),
            source: err,
        })
}

fn slash_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// `strconv.Quote` 的窄口径等价物（见模块文档）。
#[must_use]
pub fn go_quote(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            other if other.is_control() => {
                out.push_str(&format!("\\u{:04x}", other as u32));
            }
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TestDir(PathBuf);

    impl TestDir {
        fn new(name: &str) -> Self {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_nanos();
            let dir = std::env::temp_dir().join(format!("mc-daemon-skill-policy-{name}-{nanos:x}"));
            fs::create_dir_all(&dir).expect("create test dir");
            Self(dir)
        }

        fn path(&self) -> &Path {
            self.0.as_path()
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn skill(name: &str) -> SkillForEnv {
        SkillForEnv {
            name: name.to_string(),
            content: String::new(),
        }
    }

    fn disabled(root: &str, key: &str, name: &str) -> RuntimeSkillRefForEnv {
        RuntimeSkillRefForEnv {
            root: root.to_string(),
            key: key.to_string(),
            name: name.to_string(),
            plugin: String::new(),
        }
    }

    #[test]
    fn clean_key_rejects_only_the_documented_shapes() {
        assert_eq!(clean_runtime_skill_key(" a/b ").as_deref(), Some("a/b"));
        assert_eq!(clean_runtime_skill_key("a//b").as_deref(), Some("a/b"));
        // ⚠️ 与 normalize_local_skill_key 的差别：`..foo` 在这里是合法的。
        assert_eq!(clean_runtime_skill_key("..foo").as_deref(), Some("..foo"));
        for bad in [".", "/abs", "..", "../x"] {
            assert_eq!(clean_runtime_skill_key(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn claude_settings_emit_both_override_and_deny_rules() {
        let dir = TestDir::new("claude-settings");
        let path = prepare_claude_skill_settings(
            dir.path(),
            &[disabled("provider", "review", "Review")],
            &[],
        )
        .expect("write")
        .expect("some path");
        let body: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).expect("read")).expect("parse");
        assert_eq!(body["skillOverrides"]["Review"], "off");
        assert_eq!(body["permissions"]["deny"][0], "Skill(Review)");
        assert_eq!(body["permissions"]["deny"][1], "Skill(Review *)");
    }

    #[test]
    fn plugin_skills_get_a_deny_rule_only_and_use_the_key_as_the_name() {
        let dir = TestDir::new("claude-plugin");
        let path = prepare_claude_skill_settings(
            dir.path(),
            &[disabled("plugin", "design", "Design")],
            &[],
        )
        .expect("write")
        .expect("some");
        let body: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).expect("read")).expect("parse");
        assert_eq!(body["skillOverrides"], serde_json::json!({}));
        assert_eq!(body["permissions"]["deny"][0], "Skill(design)");
    }

    #[test]
    fn workspace_skills_shadow_runtime_skills() {
        let dir = TestDir::new("claude-shadow");
        let disabled_skill = disabled("provider", "review", "PR review");
        // 显示名被 workspace skill 占用 ⇒ 整条跳过 ⇒ 没有 sidecar。
        let path =
            prepare_claude_skill_settings(dir.path(), &[disabled_skill], &[skill("pr-review")])
                .expect("write");
        assert!(path.is_none());
        assert!(!dir.path().join(CLAUDE_RUNTIME_SKILL_SETTINGS_FILE).exists());
    }

    #[test]
    fn an_empty_disable_list_removes_the_previous_settings_file() {
        let dir = TestDir::new("claude-remove");
        let written =
            prepare_claude_skill_settings(dir.path(), &[disabled("provider", "a", "A")], &[])
                .expect("write")
                .expect("some");
        assert!(written.exists());
        let path = prepare_claude_skill_settings(dir.path(), &[], &[]).expect("cleanup");
        assert!(path.is_none());
        assert!(!written.exists());
    }

    #[test]
    fn codex_config_appends_one_block_per_disabled_skill() {
        let dir = TestDir::new("codex-append");
        let config = dir.path().join("config.toml");
        fs::write(&config, "[profile.default]\n").expect("seed");

        ensure_codex_disabled_skills_config(
            &config,
            Path::new("/codex-home"),
            &[
                disabled("provider", "release/reporter", "reporter"),
                // 同一路径写两次只落一个块。
                disabled("provider", "release/reporter", "reporter"),
                // 非 provider/universal 的根跳过。
                disabled("plugin", "design", "design"),
            ],
            &[],
        )
        .expect("append");

        let body = fs::read_to_string(&config).expect("read");
        assert!(body.starts_with("[profile.default]\n"));
        assert_eq!(body.matches("[[skills.config]]").count(), 1);
        assert!(body.contains("path = \"/codex-home/skills/release/reporter/SKILL.md\""));
        assert!(body.contains("enabled = false"));
    }

    #[test]
    fn codex_config_skips_keys_claimed_by_a_workspace_skill() {
        let dir = TestDir::new("codex-claim");
        let config = dir.path().join("config.toml");
        ensure_codex_disabled_skills_config(
            &config,
            Path::new("/codex-home"),
            &[disabled("provider", "review", "review")],
            &[skill("Review")],
        )
        .expect("append");
        assert!(!config.exists(), "nothing to write ⇒ no file created");
    }

    #[test]
    fn go_quote_escapes_the_documented_characters() {
        assert_eq!(go_quote("plain/path.md"), "\"plain/path.md\"");
        assert_eq!(go_quote("a\"b"), "\"a\\\"b\"");
        assert_eq!(go_quote("a\\b"), "\"a\\\\b\"");
        assert_eq!(go_quote("a\nb"), "\"a\\nb\"");
        assert_eq!(go_quote("a\u{1}b"), "\"a\\u0001b\"");
    }
}
