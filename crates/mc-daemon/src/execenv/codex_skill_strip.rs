//! 剥掉 Codex `config.toml` 里的 `[[skills.config]]` 块。
//!
//! - **上游**：`execenv/codex_skill_strip.go`（87 行）。
//! - **写者**：M6-9（本 slice）。
//!
//! ## 为什么要剥
//!
//! Codex Desktop 会给它认识的每个 skill 写一条 `[[skills.config]]`：文件型 skill 有
//! `path = "..."`，而插件型 skill（例如 `name = "superpowers:brainstorming"`）只有 `name`。
//! Codex CLI 0.114 的 TOML 反序列化把 `path` 当**必填**，于是那些插件条目会让它报
//! `missing field path` 并**拒绝启动**。Multica 把用户的 `~/.codex/config.toml` 原样拷进每个
//! 任务的隔离 codex-home，于是这份坏条目被传播进 per-task 配置，`codex thread/start` 就挡住了。
//!
//! 整段 `[[skills.config]]` 一起剥掉即可：Multica 把当前指派给 agent 的 skill **直接写到**
//! `codex-home/skills/<name>/SKILL.md`，Codex 从那个目录自动发现它们；用户级 skill 注册表对
//! 一次 per-task 运行毫无关系，所以丢掉它既安全、也正好是隔离该有的范围。
//!
//! 块外的行**一个字节都不动**。
//!
//! ## 与上游的差异
//!
//! 无。这是纯字符串变换 + 一次原地重写，逐行照抄（含「去掉尾部空行簇 + 补一个 `\n`」与
//! 「整篇只剩空白 ⇒ 空串」两条收尾规则）。

use std::fs;
use std::path::Path;

use crate::execenv::sidecar::SidecarError;

/// TOML 数组表块头。
const SKILLS_CONFIG_HEADER: &str = "[[skills.config]]";

/// 剥掉所有 `[[skills.config]]` 块（上游 `stripSkillsConfigEntries`）。
#[must_use]
pub fn strip_skills_config_entries(content: &str) -> String {
    if !content.contains(SKILLS_CONFIG_HEADER) {
        return content.to_string();
    }

    let mut out: Vec<&str> = Vec::new();
    let mut in_skills_config = false;
    for line in content.split('\n') {
        let trimmed = line.trim();

        // 任何新的 TOML 表头都结束当前 `[[skills.config]]` 块 —— 无论是同一个数组的下一项
        // 还是另一张表。
        if trimmed.starts_with('[') {
            if trimmed == SKILLS_CONFIG_HEADER {
                in_skills_config = true;
                continue;
            }
            in_skills_config = false;
            out.push(line);
            continue;
        }

        if in_skills_config {
            continue;
        }
        out.push(line);
    }

    let stripped = out.join("\n");
    // 去掉尾部空行簇（否则反复拷贝会让文件无限增长），再补回一个换行。
    let mut stripped = stripped.trim_end_matches('\n').to_string();
    stripped.push('\n');
    if stripped.trim().is_empty() {
        return String::new();
    }
    stripped
}

/// 就地重写 per-task 的 `config.toml`（上游 `sanitizeCopiedCodexConfig`）。
///
/// 文件不存在、或剥完没变 ⇒ no-op（**不**制造 mtime 抖动）。
pub fn sanitize_copied_codex_config(config_path: &Path) -> Result<(), SidecarError> {
    let data = match fs::read_to_string(config_path) {
        Ok(data) => data,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => {
            return Err(SidecarError::Io {
                op: "read config.toml",
                path: config_path.to_path_buf(),
                source: err,
            });
        }
    };
    let stripped = strip_skills_config_entries(&data);
    if stripped == data {
        return Ok(());
    }
    fs::write(config_path, stripped).map_err(|err| SidecarError::Io {
        op: "write config.toml",
        path: config_path.to_path_buf(),
        source: err,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn content_without_the_header_is_returned_untouched() {
        let content = "[profile.default]\nmodel = \"gpt\"\n";
        assert_eq!(strip_skills_config_entries(content), content);
    }

    #[test]
    fn a_single_block_is_removed_and_the_rest_is_preserved() {
        let content = "[profile.default]\nmodel = \"gpt\"\n\n[[skills.config]]\npath = \"/x/SKILL.md\"\nenabled = false\n\n[mcp_servers.a]\ncommand = \"x\"\n";
        let stripped = strip_skills_config_entries(content);
        assert!(!stripped.contains("[[skills.config]]"));
        assert!(!stripped.contains("/x/SKILL.md"));
        assert!(stripped.contains("[profile.default]"));
        assert!(stripped.contains("[mcp_servers.a]"));
        assert!(stripped.ends_with('\n'));
    }

    #[test]
    fn consecutive_blocks_are_all_removed() {
        let content = "[[skills.config]]\npath = \"a\"\n\n[[skills.config]]\nname = \"b\"\n\n[[skills.config]]\npath = \"c\"\n\n[other]\nx = 1\n";
        let stripped = strip_skills_config_entries(content);
        assert!(!stripped.contains("skills.config"));
        assert_eq!(stripped.matches("[other]").count(), 1);
        assert!(stripped.contains("x = 1"));
    }

    #[test]
    fn repeated_stripping_does_not_grow_the_file() {
        let content = "[a]\nx = 1\n\n[[skills.config]]\npath = \"p\"\n";
        let once = strip_skills_config_entries(content);
        let twice = strip_skills_config_entries(&once);
        assert_eq!(once, twice);
        assert_eq!(once, "[a]\nx = 1\n");
    }

    #[test]
    fn an_all_skills_config_document_becomes_empty() {
        let content = "[[skills.config]]\npath = \"a\"\n";
        assert_eq!(strip_skills_config_entries(content), "");
    }

    #[test]
    fn sanitize_rewrites_only_when_something_changed() {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("mc-daemon-codex-strip-{nanos:x}"));
        fs::create_dir_all(&dir).expect("mkdir");
        let config = dir.join("config.toml");

        // 不存在 ⇒ no-op。
        sanitize_copied_codex_config(&config).expect("missing is fine");
        assert!(!config.exists());

        fs::write(
            &config,
            "[profile.default]\nmodel = \"gpt\"\n\n[[skills.config]]\nname = \"superpowers:brainstorming\"\n",
        )
        .expect("write");
        sanitize_copied_codex_config(&config).expect("strip");
        let body = fs::read_to_string(&config).expect("read");
        assert!(!body.contains("skills.config"));
        assert!(body.contains("model = \"gpt\""));
        let modified = fs::metadata(&config)
            .expect("metadata")
            .modified()
            .expect("mtime");

        // 再跑一次：没变化 ⇒ 不重写（mtime 不变是「no-op」的可观测证据）。
        sanitize_copied_codex_config(&config).expect("second strip");
        assert_eq!(
            fs::metadata(&config)
                .expect("metadata")
                .modified()
                .expect("mtime"),
            modified
        );

        let _ = fs::remove_dir_all(&dir);
    }
}
