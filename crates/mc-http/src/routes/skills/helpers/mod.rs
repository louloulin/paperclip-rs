//! skill 面**共用件**：会话/工作区解析、`load_skill_for_user`、响应投影。
//!
//! - **写者**：M6-2（**W**，本文件唯一写者；`docs/57` §3.2）。M6-3 只**读**。
//!   ⚠️ M6-3 若需要一个还不存在的 helper，**加到自己的** `import.rs` / `refresh.rs`，
//!   **不要**回头改本文件（否则两片同改一个文件 —— 这正是 anchor 要消灭的东西）。
//! - **上游**：`internal/handler/skill.go` 的 `resolveWorkspaceID` / `requireWorkspaceMember`
//!   / `loadSkillForUser`（+ `skill_create.go` 的公共校验）。
//! - **本仓约定**：
//!   - 鉴权沿用 `routes::auth_user::AuthUser` 提取器 + `workspace_role` 家族查询；
//!   - 跨工作区 / 非成员一律 **404**（不是 403）—— 与上游一致，避免探测存在性；
//!   - `load_skill_for_user` 是**唯一**的取 skill 入口：所有子文件（含 M6-3 的 import/refresh）
//!     都要走它，不要各写一份带不同过滤条件的版本；
//!   - DTO 在这里统一投影（`skill` 行 → 响应），**不要**把行结构直接 `Serialize`
//!     （列名与响应字段名不一致，且 `content` 在列表响应里应省略）。
//! - **不做什么**：不在这里做保留路径 / 二进制判定（`mc-skill` 的纯函数）、不做 bundle 哈希
//!   （`routes/daemon/skills.rs`，M6-4）。
//!
//! **状态：M6-2 已落地（LUM-1667）**。门 ⑩ 行预算：桩写「260 行」，落地约 **790 行**
//! （硬限 800 ✓）——多出来的几乎全是**投影层**：12 个响应形状 + 4 个请求体，加上
//! `ClawHub` 客户端与两条 Go 转义函数。

use std::collections::HashMap;
use std::fmt::Write as _;
use std::time::Duration;

use axum::body::Bytes;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use axum::Json;
use chrono::{DateTime, Utc};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use sha2::{Digest, Sha256};

use mc_core::Id;
use mc_errors::Error;
use mc_repos::skill::read::{
    SkillFileMetadataRow, SkillFileRow, SkillLabelRow, SkillRepo, SkillRow, SkillSummaryRow,
};
use mc_repos::skill::write::{NewSkill, SkillFileInput, SkillUpdate};

use crate::routes::agents::{bad_request, forbidden, not_found, parse_uuid, repo_err};
use crate::routes::inbox::resolve_workspace_id;
use crate::state::AppState;

// ---------------------------------------------------------------------------
// 响应投影
// ---------------------------------------------------------------------------

fn ts(t: DateTime<Utc>) -> String {
    t.to_rfc3339()
}

fn ts_2(a: DateTime<Utc>, b: DateTime<Utc>) -> (String, String) {
    (ts(a), ts(b))
}

/// `skill.config` 的规范化（上游 `decodeSkillConfig`）：`null` ⇒ `{}`。
///
/// 迁移里该列是 `NOT NULL DEFAULT '{}'`，所以真实行不会是 `null`；但 `SQL NULL`
/// 与 JSON `null` 在 jsonb 里长得一样，上游为此显式兜了一层，这里照抄。
fn normalise_config(config: &JsonValue) -> JsonValue {
    if config.is_null() {
        serde_json::json!({})
    } else {
        config.clone()
    }
}

mod dto;
pub(crate) use dto::*;

// ---------------------------------------------------------------------------
// 路径 / 内容校验
// ---------------------------------------------------------------------------

mod validate;
pub(crate) use validate::*;

// ---------------------------------------------------------------------------
// 调用上下文
// ---------------------------------------------------------------------------

mod scope;
pub(crate) use scope::*;

// ---------------------------------------------------------------------------
// ClawHub 搜索客户端（上游 `searchClawHubSkills`）
// ---------------------------------------------------------------------------

mod clawhub;
pub(crate) use clawhub::*;

// ---------------------------------------------------------------------------
// 转义（本 crate 没有 `url` 依赖 ⇒ 手写 Go 的两条最小转义）
// ---------------------------------------------------------------------------

mod escape;
pub(crate) use escape::*;

#[cfg(test)]
mod tests {
    use super::*;

    fn q(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    #[test]
    fn include_defaults_to_content_and_rejects_anything_else() {
        // `mc_errors::Error` 没有 `PartialEq` ⇒ 比 `Result` 要解出来各自断言
        assert!(resolve_include(&q(&[])).unwrap());
        assert!(resolve_include(&q(&[("include", "")])).unwrap());
        assert!(resolve_include(&q(&[("include", " content ")])).unwrap());
        assert!(!resolve_include(&q(&[("include", "metadata")])).unwrap());
        let err = resolve_include(&q(&[("include", "files")])).unwrap_err();
        assert_eq!(err.http_status(), axum::http::StatusCode::BAD_REQUEST);
    }

    #[test]
    fn content_hash_is_bare_hex_sha256_of_the_bytes() {
        // 与 SQL `encode(sha256(convert_to(content,'UTF8')),'hex')` 同值（不带 `sha256:` 前缀）。
        assert_eq!(
            content_hash(""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            content_hash("hello"),
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }

    #[test]
    fn escapes_follow_go_query_and_path_rules() {
        // Go：`QueryEscape` 空格→`+`、`+`→`%2B`；`PathEscape` 空格→`%20`、`/`→`%2F`。
        assert_eq!(query_escape("a b+c/d&e"), "a+b%2Bc%2Fd%26e");
        assert_eq!(path_escape("a b+c/d&e"), "a%20b+c%2Fd&e");
        assert_eq!(query_escape("react"), "react");
    }

    #[test]
    fn clawhub_url_skips_the_owner_segment_when_absent() {
        assert_eq!(
            clawhub_skill_url("acme", "react-tips"),
            "https://clawhub.ai/acme/react-tips"
        );
        assert_eq!(
            clawhub_skill_url("", "react-tips"),
            "https://clawhub.ai/react-tips"
        );
    }

    #[test]
    fn config_null_normalises_to_an_empty_object() {
        assert_eq!(normalise_config(&JsonValue::Null), serde_json::json!({}));
        assert_eq!(
            normalise_config(&serde_json::json!({"a": 1})),
            serde_json::json!({"a": 1})
        );
    }
}
