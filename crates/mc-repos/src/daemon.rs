//! daemon 面仓储（M3-7 / LUM-1438）。
//!
//! 覆盖 `/api/daemon/*` 需要的 SQL：runtime 注册/在线、daemon token、
//! task 生命周期（start/wait/complete/fail/cancel-ack/pin-session）、task_message 追加、
//! 以及 GC 探针的只读投影。
//!
//! ## 为什么不全走 `mc-task::TaskStore`
//!
//! `mc-task` 的端口只建模到 [`mc_task::state::ColumnWrite`] 为止：那里有
//! `completed_at` / `failure_reason` / `error` / `started_at` / `wait_reason` /
//! `prepare_lease_expires_at`，**没有** `result` / `session_id` / `work_dir` /
//! `durable_work_dir` / `branch_name` / `session_rollout_missing` /
//! `retired_session_id`。上游 `CompleteAgentTask`（`agent.sql:1002`）与
//! `FailAgentTask`（`agent.sql:1251`）要写全部这些列，所以这里按上游 SQL 逐字落一条
//! 专用语句；纯状态迁移（start / `waiting_local_directory`）仍与 `ColumnWrite` 对齐。
//!
//! ## 判别口径
//!
//! 所有"可能查无此行"的方法返回 `Result<Option<_>>`：
//! `Ok(None)` 表示**确认不存在**（上游 `isNotFound`，守卫据此 404），
//! `Err(_)` 表示基础设施故障（守卫必须落 500，见 `docs/16` §7.1 的 MUL-7259）。

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::{FromRow as _, PgPool, Row as _};
use uuid::Uuid;

use mc_core::Id;

use crate::runtime::AGENT_RUNTIME_COLUMNS;
use mc_db::Db;

use crate::runtime::AgentRuntimeRow;
use crate::task::TaskMessageRow;
use crate::workspace::map_sqlx_err;
use crate::{RepoError, Result};

mod gc;
mod registry;
mod skills;
mod tasks;

/// `i64` 秒 → `f64`（`make_interval(secs => …)` 吃 double precision）。
///
/// 秒数是**小整数**（租约 120 / 心跳 90 / token TTL 86400 这一量级），
/// 远够不着 `f64` 的 52 位尾数精度；与 `task/store.rs` 的同名 helper 同口径。
#[allow(clippy::cast_precision_loss)]
pub(super) fn secs_f64(secs: i64) -> f64 {
    secs as f64
}

/// `/api/daemon/*` 的仓储。
#[derive(Debug, Clone)]
pub struct DaemonRepo {
    pool: PgPool,
}

/// `skill` 一行的投影（`migrations/upstream/008_structured_skills.up.sql`）。
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct SkillRow {
    /// `id`。
    pub id: Uuid,
    /// `workspace_id`。
    pub workspace_id: Uuid,
    /// `name`（`UNIQUE(workspace_id, name)`）。
    pub name: String,
    /// `description`。
    pub description: String,
    /// `content` —— SKILL.md 正文。
    pub content: String,
    /// `config` JSONB。
    pub config: Value,
    /// `created_by` —— 本地导入的 overwrite 只有 creator 本人可做。
    pub created_by: Option<Uuid>,
    /// `created_at`。
    pub created_at: DateTime<Utc>,
    /// `updated_at`。
    pub updated_at: DateTime<Utc>,
}

impl SkillRow {
    /// `id` 的领域类型。
    #[must_use]
    pub fn id(&self) -> Id {
        Id::from(self.id)
    }
}

/// `skill_file` 一行的投影。
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct SkillFileRow {
    /// `id`。
    pub id: Uuid,
    /// `skill_id`。
    pub skill_id: Uuid,
    /// `path`（相对路径，`UNIQUE(skill_id, path)`）。
    pub path: String,
    /// `content`。
    pub content: String,
    /// `created_at`。
    pub created_at: DateTime<Utc>,
    /// `updated_at`。
    pub updated_at: DateTime<Utc>,
}

/// skill + 其支持文件（`GET …/skill-bundles/resolve` 的返回单元）。
#[derive(Debug, Clone)]
pub struct SkillBundleRow {
    /// `skill` 行。
    pub skill: SkillRow,
    /// `(path, content)` 对，按 `path` 升序。
    pub files: Vec<(String, String)>,
}

/// skill + 其**完整文件行**（本地导入的 `*/result` 回执单元）。
///
/// 与 [`SkillBundleRow`] 的区别是这里保留行本身的 id / 时间戳：`…/import/{requestId}`
/// 的 200 体是 upstream `SkillWithFilesResponse`（`skill.go:133`），其 `files` 是
/// `SkillFileResponse` 数组（含 `id`/`skill_id`/`created_at`/`updated_at`）、**无 omitempty**。
#[derive(Debug, Clone)]
pub struct SkillWithFilesRow {
    /// `skill` 行。
    pub skill: SkillRow,
    /// 支持文件行，按 `path` 升序。
    pub files: Vec<SkillFileRow>,
}

/// `overwrite_skill_with_files` 的三种干净失败（上游同名守卫的对应物）。
#[derive(Debug)]
pub enum OverwriteOutcome {
    /// 已更新。
    Updated(Box<SkillWithFilesRow>),
    /// 目标 skill 已不存在。
    Missing,
    /// 目标名字已被别人改成别的（`UNIQUE(workspace_id, name)` 语义漂移）。
    NameMismatch,
    /// 目标 skill 的 creator 不是本次导入的发起者。
    NotOwner,
}

/// `daemon_token` 一行的鉴权投影。
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct DaemonTokenRow {
    /// `id`。
    pub id: Uuid,
    /// `workspace_id` —— token 绑定的 workspace（daemon token 路径的唯一作用域来源）。
    pub workspace_id: Uuid,
    /// `daemon_id`。
    pub daemon_id: String,
    /// `expires_at`。
    pub expires_at: DateTime<Utc>,
}

impl DaemonTokenRow {
    /// `id` 的领域类型。
    #[must_use]
    pub fn id(&self) -> Id {
        Id::from(self.id)
    }

    /// `workspace_id` 的领域类型。
    #[must_use]
    pub fn workspace_id(&self) -> Id {
        Id::from(self.workspace_id)
    }
}

/// `POST /api/daemon/register` 里单个 runtime 的 upsert 入参（upstream `UpsertAgentRuntimeWithProfile`）。
#[derive(Debug, Clone)]
pub struct UpsertRuntime {
    /// token 绑定的 workspace（已解析成 UUID）。
    pub workspace_id: Id,
    /// daemon 上报的机器标识。
    pub daemon_id: String,
    /// runtime 展示名（空名由调用方回落到 provider）。
    pub name: String,
    /// provider（已 `normalizeProvider`）。
    pub provider: String,
    /// `runtime_mode`（上游固定写 `"local"`）。
    pub runtime_mode: String,
    /// 注册时上报的状态：`"online"`，或 daemon 自报 `"offline"`。
    pub status: String,
    /// 机器名（落 `device_info`）。
    pub device_info: String,
    /// `metadata` JSONB。
    pub metadata: Value,
    /// 归属用户；`None` 走 `COALESCE` 保留既有 owner（daemon token 路径）。
    pub owner_id: Option<Id>,
    /// 绑定的 runtime profile。
    pub profile_id: Option<Id>,
}

/// upsert runtime 的结果：行 + 上游 `(xmax = 0) AS inserted`。
#[derive(Debug, Clone)]
pub struct RuntimeUpsert {
    /// 写入/更新后的 runtime 行。
    pub row: AgentRuntimeRow,
    /// `true` = 本次是新插入（不是更新）。
    pub inserted: bool,
}

/// upstream `agent.ProfileRuntimeType`：`runtime_type` 非空取它，否则回退 `protocol_family`。
///
/// `mc-repos` 不能依赖 `mc-http` 的同名派生函数（层次），而这里只需要这一条回退规则。
fn profile_runtime_type(runtime_type: &str, protocol_family: &str) -> String {
    if runtime_type.trim().is_empty() {
        protocol_family.to_string()
    } else {
        runtime_type.to_string()
    }
}

/// workspace 的 repos/settings 投影（upstream `workspaceReposResponse`）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorkspaceRepos {
    /// workspace id（字符串形，上游回字符串）。
    pub workspace_id: String,
    /// `workspace.repos` 归一化后的数组。
    pub repos: Value,
    /// 由 `repos[].url` 派生的版本号（与上游同算法，见 [`repos_version`]）。
    pub repos_version: String,
    /// `workspace.settings` 原样透传（空对象时省略，与上游 `omitempty` 一致）。
    pub settings: Option<Value>,
}

/// GC 探针需要的 issue 投影（upstream `ListIssueGCStatuses`）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct IssueGcRow {
    /// issue id。
    pub id: Id,
    /// 原始状态键。
    pub status: String,
    /// 生命周期类别（upstream `issuestatus` 四值词汇；空串 = 无类别，见
    /// [`issue_category`] 的说明）。
    pub category: String,
    /// `updated_at`。
    pub updated_at: Option<DateTime<Utc>>,
}

/// GC 探针需要的 chat session 投影（upstream `GetChatSession`）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChatSessionGcRow {
    /// session id。
    pub id: Id,
    /// 所属 workspace（守卫用）。
    pub workspace_id: Id,
    /// 状态。
    pub status: String,
    /// `updated_at`。
    pub updated_at: Option<DateTime<Utc>>,
}

/// GC 探针需要的 autopilot run 投影（upstream `GetAutopilotRun` + 父 `GetAutopilot`）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AutopilotRunGcRow {
    /// run id。
    pub id: Id,
    /// 父 autopilot 的 workspace（守卫用；父行不存在时调用方落 404）。
    pub workspace_id: Option<Id>,
    /// 状态。
    pub status: String,
    /// `completed_at`。
    pub completed_at: Option<DateTime<Utc>>,
}

/// 单 issue GC 探针的投影（upstream `GetIssueGCStatus`）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct IssueGcProbeRow {
    /// issue id。
    pub id: Id,
    /// 所属 workspace（反枚举门用：不匹配一律 404）。
    pub workspace_id: Id,
    /// 原始状态键。
    pub status: String,
    /// `updated_at`。
    pub updated_at: DateTime<Utc>,
}

/// `task_usage` 的 upsert 入参（upstream `UpsertTaskUsage`，`UNIQUE (task_id, provider, model)`）。
#[derive(Debug, Clone)]
pub struct TaskUsageUpsert {
    /// 归账 task。
    pub task_id: Id,
    /// provider（调用方已 `normalizeProvider`）。
    pub provider: String,
    /// 模型名。
    pub model: String,
    /// 输入 token。
    pub input_tokens: i64,
    /// 输出 token。
    pub output_tokens: i64,
    /// 缓存读 token。
    pub cache_read_tokens: i64,
    /// 缓存写 token。
    pub cache_write_tokens: i64,
    /// provider 自报价格（1e-10 USD）；`None` = 未上报（读侧回落费率表估算）。
    pub cost_usd_ticks: Option<i64>,
}

/// 一条待入库的 `task_message`（upstream `InsertTaskMessage`）。
#[derive(Debug, Clone)]
pub struct NewTaskMessage {
    /// 归账 task。
    pub task_id: Id,
    /// 批内序号。
    pub seq: i32,
    /// 消息类型（`tool` / `text` / …）。
    pub kind: String,
    /// 工具名。
    pub tool: Option<String>,
    /// 文本内容。
    pub content: Option<String>,
    /// 工具入参。
    pub input: Option<Value>,
    /// 工具输出。
    pub output: Option<String>,
    /// 输出是否被截断。
    pub output_truncated: Option<bool>,
    /// 工具调用 id。
    pub call_id: Option<String>,
    /// 一个 daemon 观测到的事件时间（`None` ⇒ 落 `now()`）。
    ///
    /// upstream 用 `NULLIF($n,'')` 走 text[] 传参：任一值缺失或与服务器时钟
    /// 偏离超过 2 分钟，**整批**都退回数据库时间，避免成对事件（`tool_call` /
    /// `tool_result`）混用两个时钟。本层保留同一语义：`None` ⇒ `COALESCE(..., now())`。
    pub created_at: Option<DateTime<Utc>>,
}
impl DaemonRepo {
    /// 从应用共享 `Db` 句柄构造。
    #[must_use]
    pub fn new(db: &Db) -> Self {
        Self {
            pool: db.pool().clone(),
        }
    }

    /// 从裸连接池构造（测试用）。
    #[must_use]
    pub fn from_pool(pool: PgPool) -> Self {
        Self { pool }
    }

    /// 连接池引用。
    #[must_use]
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }
}

/// 上游 `workspaceReposVersion`（`daemon.go:253`）：非空 url 排序后按 `\n` 连接取 sha256-hex。
#[must_use]
pub fn repos_version(repos: &Value) -> String {
    let mut urls: Vec<&str> = repos
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.get("url").and_then(Value::as_str))
                .filter(|url| !url.is_empty())
                .collect()
        })
        .unwrap_or_default();
    urls.sort_unstable();
    let mut hasher = Sha256::new();
    hasher.update(urls.join("\n").as_bytes());
    let digest = hasher.finalize();
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// 上游 `parseWorkspaceRepos` + `normalizeWorkspaceRepos`：非数组 / 解析失败一律回落
/// 空数组（不报错）；每个条目的 `url` 去空白，空白 url 丢弃，**重复 url 只留首个**
/// （上游按出现顺序去重，不是排序去重）。
#[must_use]
pub fn normalize_workspace_repos(raw: &Value) -> Value {
    let Value::Array(items) = raw else {
        return Value::Array(Vec::new());
    };
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut out: Vec<Value> = Vec::with_capacity(items.len());
    for item in items {
        let Some(url) = item.get("url").and_then(Value::as_str) else {
            continue;
        };
        let url = url.trim();
        if url.is_empty() || !seen.insert(url.to_string()) {
            continue;
        }
        let mut item = item.clone();
        if let Value::Object(map) = &mut item {
            map.insert("url".to_string(), Value::String(url.to_string()));
        }
        out.push(item);
    }
    Value::Array(out)
}

/// 内置 issue 状态 → GC 生命周期类别（上游 `issuestatus.WireCategory` 的本仓投影）。
///
/// GC 只消费一个事实（"这个 issue 终结了吗"）：`done`/`cancelled` 终结，
/// 事务内读某 skill 的支持文件（`path` 升序）。
///
/// 本地导入的两个写路径都要把刚写进去的文件行原样回给 daemon（`files` **无**
/// omitempty），所以读写必须在同一个事务里完成，不能写完了再去池上查一次 ——
/// 那样会看到并发更新的文件集，与本次写入的 skill 行不匹配。
async fn skill_files(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    skill_id: Uuid,
) -> Result<Vec<SkillFileRow>> {
    sqlx::query_as::<_, SkillFileRow>(
        "SELECT id, skill_id, path, content, created_at, updated_at FROM skill_file \
         WHERE skill_id = $1 ORDER BY path",
    )
    .bind(skill_id)
    .fetch_all(&mut **tx)
    .await
    .map_err(map_sqlx_err)
}

/// 内置状态键 → upstream `issuestatus` 的四值生命周期类别。
///
/// 返回 `""` 表示**无类别**（不是 "unknown"）：上游 `issueGCWire` 对无类别的
/// 状态原样回传 `status` 且**省略** `category`，让 daemon fail-closed（只回收产物）。
/// 本仓只支持 7 个内置状态，这些键全部有类别；回落分支留给未来自定义状态。
#[must_use]
pub fn issue_category(status: &str) -> &'static str {
    match status {
        "backlog" | "todo" => "unstarted",
        "in_progress" | "in_review" | "blocked" => "started",
        "done" => "done",
        "cancelled" => "closed",
        _ => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn repos_version_matches_upstream_algorithm() {
        // 空数组 ⇒ sha256("") = e3b0c442...
        assert_eq!(
            repos_version(&json!([])),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        // 排序 + 跳过空 url：顺序无关，空 url 不参与。
        let a = repos_version(&json!([
            {"url": "https://github.com/b/b.git"},
            {"url": ""},
            {"url": "https://github.com/a/a.git"}
        ]));
        let b = repos_version(&json!([
            {"url": "https://github.com/a/a.git"},
            {"url": "https://github.com/b/b.git"}
        ]));
        assert_eq!(a, b);
        assert_eq!(a.len(), 64);
    }

    #[test]
    fn normalize_workspace_repos_rejects_non_array() {
        assert_eq!(normalize_workspace_repos(&json!({"a": 1})), json!([]));
        assert_eq!(normalize_workspace_repos(&json!(null)), json!([]));
        assert_eq!(
            normalize_workspace_repos(&json!([{"url": "u"}, {"no": "url"}])),
            json!([{"url": "u"}])
        );
    }

    #[test]
    fn normalize_workspace_repos_trims_and_dedupes_in_order() {
        assert_eq!(
            normalize_workspace_repos(&json!([
                {"url": "  b  "},
                {"url": "a"},
                {"url": "b"},
                {"url": "   "},
                {"url": "a"}
            ])),
            json!([{"url": "b"}, {"url": "a"}])
        );
    }

    #[test]
    fn issue_category_projects_builtin_keys() {
        assert_eq!(issue_category("backlog"), "unstarted");
        assert_eq!(issue_category("todo"), "unstarted");
        assert_eq!(issue_category("in_progress"), "started");
        assert_eq!(issue_category("in_review"), "started");
        assert_eq!(issue_category("blocked"), "started");
        assert_eq!(issue_category("done"), "done");
        assert_eq!(issue_category("cancelled"), "closed");
        // 无类别（不是 "unknown"）：上游据此省略 `category` 并原样回传 status。
        assert_eq!(issue_category("whatever-custom"), "");
    }
}
