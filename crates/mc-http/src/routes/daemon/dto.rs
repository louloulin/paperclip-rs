//! daemon 面的请求 / 响应体（R7 拆分自 `daemon.rs`）。
//!
//! ## 忠实度口径
//!
//! 每个结构体都对着上游某一个 Go 结构体写，**字段名逐字相同**（serde 的默认
//! `snake_case` 与 Go 的 `json:"..."` 恰好一致）；刻意不做的部分逐条登记在
//! `docs/32-M3-DAEMON-FACE.md` 的偏离表里，不在代码里静默省略。
//!
//! 关键三类：
//!
//! 1. [`RegisterRequest`] / [`HeartbeatRequest`] / [`DaemonDeregisterRequest`] —— HTTP 面请求体。
//!    WS 面复用 `mc-daemon-proto` 的同名结构（两条传输共用一套语义）。
//! 2. [`DaemonTaskResponse`] —— upstream `taskToResponse`（`agent.go:797`）的逐字段投影。
//!    上游那个 Go 结构体有 100+ 字段，但 `taskToResponse` **只填**这里列出的这些，
//!    其余留在零值；claim 路径另外补 `auth_token` / `remote_mcp_daemon_token`。
//!    `agent.*` / `repos` / `skills` 等由别的 builder 填（本切片不实现，见偏离表）。
//! 3. [`WorkspaceRepos`] 直接复用 `mc-repos` 的投影 —— 它就是
//!    `workspaceReposResponse`，无需再包一层。

use mc_repos::task::TaskRow;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

use super::scope::{timestamp, timestamp_opt};

// ---------------------------------------------------------------------------
// 请求体
// ---------------------------------------------------------------------------

/// upstream `DaemonRegisterRequest`（`daemon.go:196`）。
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct RegisterRequest {
    /// 目标 workspace（字符串 UUID）。
    #[serde(default)]
    pub(crate) workspace_id: String,
    /// 本机 daemon id。
    #[serde(default)]
    pub(crate) daemon_id: String,
    /// 历史 hostname 派生的 daemon id（迁移用）。
    #[serde(default)]
    pub(crate) legacy_daemon_ids: Vec<String>,
    /// 机器名。
    #[serde(default)]
    pub(crate) device_name: String,
    /// multica CLI 版本。
    #[serde(default)]
    pub(crate) cli_version: String,
    /// `"desktop"` 表示由 Electron 应用拉起。
    #[serde(default)]
    pub(crate) launched_by: String,
    /// 本机可用的 runtime 列表。
    #[serde(default)]
    pub(crate) runtimes: Vec<RegisterRuntime>,
    /// 解析失败的自定义 profile。
    #[serde(default)]
    pub(crate) failed_profiles: Vec<FailedProfile>,
}

/// `register.runtimes[]` 的一项（上游匿名结构体，`daemon.go:207`）。
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct RegisterRuntime {
    /// 展示名。
    #[serde(default)]
    pub(crate) name: String,
    /// **protocol family**（上游字段名就是 `type`，不是 `provider`）。
    #[serde(default, rename = "type")]
    pub(crate) kind: String,
    /// 该 CLI 自己的版本。
    #[serde(default)]
    pub(crate) version: String,
    /// daemon 自报状态（只有 `"offline"` 有特殊含义）。
    #[serde(default)]
    pub(crate) status: String,
    /// 非空 = 这是某个自定义 runtime profile 的实例。
    #[serde(default)]
    pub(crate) profile_id: String,
}

/// `register.failed_profiles[]` 的一项（上游匿名结构体，`daemon.go:218`）。
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct FailedProfile {
    /// profile id。
    #[serde(default)]
    pub(crate) profile_id: String,
    /// 解析到的命令名。
    #[serde(default)]
    pub(crate) command_name: String,
    /// 失败原因。
    #[serde(default)]
    pub(crate) reason: String,
}

/// upstream `DaemonHeartbeatRequest` —— **与 `mc-daemon-proto` 的
/// `DaemonHeartbeatRequestPayload` 逐字同义**，这里直接复用 proto 类型。
pub(crate) type HeartbeatRequest = mc_daemon_proto::messages::daemon::DaemonHeartbeatRequestPayload;

/// upstream `DaemonDeregisterRequest`（`daemon.go:939` 附近）。
///
/// `offline_reasons` 是**按请求原文 id 索引的 JSON 对象**（上游
/// `map[string]json.RawMessage`），不是数组：老的 daemon 整段不发，只有「用户必须去修」
/// 的停机才带（MUL-6164）。值统一透传给 `agent_runtime.metadata.offline_reason`，
/// 所以这里不做形状约束（裸字符串 / 对象 / `null` 都接受）。
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct DeregisterRequest {
    /// 要下线的 runtime id 列表。
    #[serde(default)]
    pub(crate) runtime_ids: Vec<String>,
    /// 可选的下线原因，按**请求里的原文 id** 索引。
    #[serde(default)]
    pub(crate) offline_reasons: HashMap<String, Value>,
}

/// upstream `ClaimTasksByRuntimeRequest`（`daemon.go:1706` 附近）。
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct BatchClaimRequest {
    /// 机器标识；必填，且必须与 token 里的 daemon id 一致。
    #[serde(default)]
    pub(crate) daemon_id: String,
    /// 要领取的 runtime id 列表。
    #[serde(default)]
    pub(crate) runtime_ids: Vec<String>,
    /// 上限；`0` = 明确不领（不折算成 1），负数 = 400。
    #[serde(default)]
    pub(crate) max_tasks: i64,
}

/// upstream `MarkTaskWaitingLocalDirectoryRequest`。
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct WaitLocalDirectoryRequest {
    /// 等待原因。
    #[serde(default)]
    pub(crate) reason: String,
}

/// upstream `TaskCompleteRequest`（`daemon.go:4175`）。
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct TaskCompleteRequest {
    /// PR 链接。
    #[serde(default, rename = "pr_url")]
    pub(crate) pr_url: String,
    /// 最终输出。
    #[serde(default)]
    pub(crate) output: String,
    /// CLI 会话 id。
    #[serde(default)]
    pub(crate) session_id: String,
    /// 工作目录。
    #[serde(default)]
    pub(crate) work_dir: String,
    /// 可持久保留的工作目录。
    #[serde(default)]
    pub(crate) durable_work_dir: String,
    /// 分支名。
    #[serde(default)]
    pub(crate) branch_name: String,
    /// 会话 rollout 文件缺失（不可续跑）。
    #[serde(default)]
    pub(crate) session_rollout_missing: bool,
    /// 被替换掉的旧会话 id。
    #[serde(default)]
    pub(crate) retired_session_id: String,
}

/// upstream `TaskFailRequest`（`daemon.go:4175` 附近）。
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct TaskFailRequest {
    /// 错误原文。
    #[serde(default)]
    pub(crate) error: String,
    /// CLI 会话 id。
    #[serde(default)]
    pub(crate) session_id: String,
    /// 工作目录。
    #[serde(default)]
    pub(crate) work_dir: String,
    /// 可持久保留的工作目录。
    #[serde(default)]
    pub(crate) durable_work_dir: String,
    /// 结构化失败原因（与 `error` 分开，UI 按它分类）。
    #[serde(default)]
    pub(crate) failure_reason: String,
    /// 分支名。
    #[serde(default)]
    pub(crate) branch_name: String,
    /// 会话 rollout 文件缺失。
    #[serde(default)]
    pub(crate) session_rollout_missing: bool,
    /// 被替换掉的旧会话 id。
    #[serde(default)]
    pub(crate) retired_session_id: String,
}

/// upstream `TaskProgressRequest`（`daemon.go:4132`）—— **只有 summary/step/total 三个字段**。
///
/// 注意：进度不写 `result` 列（上游 `ReportProgress` 只推事件与 `wait_reason`），所以
/// 这里没有 `result` 字段。
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct ProgressRequest {
    /// 进度摘要。
    #[serde(default)]
    pub(crate) summary: String,
    /// 当前步（从 1 起）。
    #[serde(default)]
    pub(crate) step: i64,
    /// 总步数。
    #[serde(default)]
    pub(crate) total: i64,
}

/// upstream `PinTaskSessionRequest`（`task_lifecycle.go:70`）。
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct PinSessionRequest {
    /// 会话 id。
    #[serde(default)]
    pub(crate) session_id: String,
    /// 工作目录。
    #[serde(default)]
    pub(crate) work_dir: String,
}

/// upstream `TaskMessageRequest`（`pkg/protocol/messages.go`）。
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct TaskMessageRequest {
    /// 工具调用 id。
    #[serde(default)]
    pub(crate) call_id: String,
    /// 流内序号。
    #[serde(default)]
    pub(crate) seq: i64,
    /// 消息类型。
    #[serde(default, rename = "type")]
    pub(crate) kind: String,
    /// 工具名。
    #[serde(default)]
    pub(crate) tool: String,
    /// 正文。
    #[serde(default)]
    pub(crate) content: String,
    /// 结构化输入。
    #[serde(default)]
    pub(crate) input: Value,
    /// 结构化输出（`tool_result` 专用；上游是**字符串**，不是 JSON）。
    #[serde(default)]
    pub(crate) output: String,
    /// 输出是否被截断（三态：缺省 = 未知，**永不**当 `false` 处理）。
    #[serde(default)]
    pub(crate) output_truncated: Option<bool>,
    /// 产生时间（缺省 = 服务端 `now()`）。
    #[serde(default)]
    pub(crate) created_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// upstream `TaskMessageBatchRequest`。
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct TaskMessageBatchRequest {
    /// 一批消息。
    #[serde(default)]
    pub(crate) messages: Vec<TaskMessageRequest>,
}

/// upstream `TaskUsagePayload`（`daemon.go:4817`）。
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct TaskUsagePayload {
    /// provider。
    #[serde(default)]
    pub(crate) provider: String,
    /// model。
    #[serde(default)]
    pub(crate) model: String,
    /// 输入 token。
    #[serde(default)]
    pub(crate) input_tokens: i64,
    /// 输出 token。
    #[serde(default)]
    pub(crate) output_tokens: i64,
    /// 缓存读 token。
    #[serde(default)]
    pub(crate) cache_read_tokens: i64,
    /// 缓存写 token。
    #[serde(default)]
    pub(crate) cache_write_tokens: i64,
    /// 成本，单位 1e-10 USD。
    #[serde(default)]
    pub(crate) cost_usd_ticks: i64,
}

/// upstream `ReportTaskUsage` 的请求体（`daemon.go:4826`：`{"usage": [...]}`）。
///
/// **空数组不是错误**：上游逐个 upsert、单条失败只 `continue`，最后一律回
/// `{"status":"ok"}`——用量是旁路观测，不能因为它失败而让任务回调变红。
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct TaskUsageRequest {
    /// 一批用量条目。
    #[serde(default)]
    pub(crate) usage: Vec<TaskUsagePayload>,
}

/// upstream `AckTaskCancelledRequest`（`daemon.go:5203`）。
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct AckTaskCancelledRequest {
    /// 取消的 worktree 任务已提交的产出分支（唯一的「工作去哪儿了」指针）。
    #[serde(default)]
    pub(crate) branch_name: String,
    /// 一次性 worktree 已被确认删除、配置的长期项目目录生效时才有值。
    #[serde(default)]
    pub(crate) durable_work_dir: String,
    /// `Finalize` 中止时的错误原文（指向被保留的 worktree）。
    #[serde(default)]
    pub(crate) error_message: String,
    /// 结构化失败原因。
    #[serde(default)]
    pub(crate) failure_reason: String,
}

/// upstream `batchIssueGCCheckRequest`（`daemon.go:5890` 附近）。
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct BatchIssueGcCheckRequest {
    /// 待探活的 issue id。
    #[serde(default)]
    pub(crate) issue_ids: Vec<String>,
}

/// upstream `batchIssueGCCheckRequest` 的**上限常量**（`daemon.go:5920`）。
pub(crate) const MAX_ISSUE_GC_BATCH_SIZE: usize = 500;

/// upstream `maxIssueGCBatchBodyBytes = 64 << 10`。
pub(crate) const MAX_ISSUE_GC_BODY_BYTES: usize = 64 << 10;

/// 空串 → `None`：上游把「没带这个字段」与「带了空串」都当**不覆盖既有值**
/// （`COALESCE` + `NULLIF`），所以在进 SQL 之前就归一成 `None`。
#[must_use]
pub(crate) fn opt(value: String) -> Option<String> {
    (!value.is_empty()).then_some(value)
}

// ---------------------------------------------------------------------------
// 任务响应（upstream `taskToResponse`，`agent.go:797`）
// ---------------------------------------------------------------------------

/// upstream `AgentTaskResponse` 中 `taskToResponse` 真正填写的那些字段。
///
/// `agent` / `repos` / `skills` / `workspace_context` / 插件钩子 / 关联 issue 状态等
/// 由一个更宽的 builder 填（不在本切片），claim 路径只用本结构体 + `auth_token`。
/// 偏离登记见 `docs/32` 的响应体缺口表。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct DaemonTaskResponse {
    /// `id`。
    pub id: String,
    /// `agent_id`。
    pub agent_id: String,
    /// `runtime_id`。
    pub runtime_id: String,
    /// `issue_id`（chat / quick-create 任务为空串）。
    pub issue_id: String,
    /// `workspace_id`。
    pub workspace_id: String,
    /// `status`。
    pub status: String,
    /// `priority`。
    pub priority: i32,
    /// `dispatched_at`。
    pub dispatched_at: Option<String>,
    /// `started_at`。
    pub started_at: Option<String>,
    /// `completed_at`。
    pub completed_at: Option<String>,
    /// `result`（原样透传 JSONB）。
    pub result: Value,
    /// `error`。
    pub error: Option<String>,
    /// `failure_reason`（上游零值是空串而不是 `null`）。
    pub failure_reason: String,
    /// `branch_name`（零值空串）。
    pub branch_name: String,
    /// `attempt`。
    pub attempt: i32,
    /// `max_attempts`。
    pub max_attempts: i32,
    /// `parent_task_id`。
    pub parent_task_id: Option<String>,
    /// `is_leader_task`。
    pub is_leader_task: bool,
    /// `created_at`。
    pub created_at: String,
    /// `trigger_comment_id`。
    pub trigger_comment_id: Option<String>,
    /// `coalesced_comment_ids`（恒数组，不为 `null`）。
    pub coalesced_comment_ids: Vec<String>,
    /// `delivered_comment_ids`（恒数组）。
    pub delivered_comment_ids: Vec<String>,
    /// `trigger_summary`。
    pub trigger_summary: Option<String>,
    /// `handoff_note`（零值空串）。
    pub handoff_note: String,
    /// `wakeup_id`（来自 `context`，无则空串）。
    pub wakeup_id: String,
    /// `work_dir`（零值空串）。
    pub work_dir: String,
    /// `relative_work_dir`。
    pub relative_work_dir: String,
    /// `durable_work_dir`（零值空串）。
    pub durable_work_dir: String,
    /// `relative_durable_work_dir`。
    pub relative_durable_work_dir: String,
    /// `chat_session_id`（零值空串）。
    pub chat_session_id: String,
    /// `autopilot_run_id`（零值空串）。
    pub autopilot_run_id: String,
    /// `kind`（`chat` / `autopilot` / `quick_create` / `comment` / `direct`）。
    pub kind: String,
    /// `cancelled_by`（未取消时为 `null`）。
    pub cancelled_by: Option<CancellationActor>,
    /// `cancelled_by_comment_change`。
    pub cancelled_by_comment_change: bool,
    /// `auth_token`：**只有 claim 路径**带；其余路径为空串（上游零值）。
    pub auth_token: String,
    /// `remote_mcp_daemon_token`（本地无插件面 ⇒ 恒空串）。
    pub remote_mcp_daemon_token: String,
}

/// upstream `TaskCancellationActor`。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct CancellationActor {
    /// `type`。
    #[serde(rename = "type")]
    pub kind: String,
    /// `id`。
    pub id: String,
    /// `name`。
    pub name: String,
}

impl DaemonTaskResponse {
    /// upstream `taskToResponse(t, workspaceID)`。
    #[must_use]
    pub(crate) fn from_row(row: &TaskRow, workspace_id: &str) -> Self {
        let work_dir = row.work_dir.clone().unwrap_or_default();
        let durable_work_dir = row.durable_work_dir.clone().unwrap_or_default();
        let task_id = row.id().as_string();
        let cancelled = row.status == "cancelled";
        Self {
            id: task_id.clone(),
            agent_id: row.agent_id().as_string(),
            runtime_id: row
                .runtime_id
                .map(|id| mc_core::Id::from(id).as_string())
                .unwrap_or_default(),
            issue_id: row
                .issue_id
                .map(|id| mc_core::Id::from(id).as_string())
                .unwrap_or_default(),
            workspace_id: workspace_id.to_string(),
            status: row.status.clone(),
            priority: row.priority,
            dispatched_at: timestamp_opt(row.dispatched_at),
            started_at: timestamp_opt(row.started_at),
            completed_at: timestamp_opt(row.completed_at),
            result: row.result.clone().unwrap_or(Value::Null),
            error: row.error.clone(),
            failure_reason: row.failure_reason.clone().unwrap_or_default(),
            branch_name: row.branch_name.clone().unwrap_or_default(),
            attempt: row.attempt,
            max_attempts: row.max_attempts,
            parent_task_id: row
                .parent_task_id
                .map(|id| mc_core::Id::from(id).as_string()),
            is_leader_task: row.is_leader_task,
            created_at: timestamp(row.created_at),
            trigger_comment_id: row
                .trigger_comment_id
                .map(|id| mc_core::Id::from(id).as_string()),
            coalesced_comment_ids: row
                .coalesced_comment_ids
                .iter()
                .map(|id| mc_core::Id::from(*id).as_string())
                .collect(),
            delivered_comment_ids: row
                .delivered_comment_ids
                .iter()
                .map(|id| mc_core::Id::from(*id).as_string())
                .collect(),
            trigger_summary: row.trigger_summary.clone(),
            handoff_note: row.handoff_note.clone().unwrap_or_default(),
            wakeup_id: context_str(row.context.as_ref(), "wakeup_id"),
            work_dir: work_dir.clone(),
            relative_work_dir: relative_work_dir(&work_dir, workspace_id, &task_id),
            durable_work_dir: durable_work_dir.clone(),
            relative_durable_work_dir: relative_work_dir(&durable_work_dir, "", ""),
            chat_session_id: row
                .chat_session_id
                .map(|id| mc_core::Id::from(id).as_string())
                .unwrap_or_default(),
            autopilot_run_id: row
                .autopilot_run_id
                .map(|id| mc_core::Id::from(id).as_string())
                .unwrap_or_default(),
            kind: compute_task_kind(row),
            cancelled_by: cancelled.then(|| CancellationActor {
                kind: row.cancelled_by_type.clone().unwrap_or_default(),
                id: row
                    .cancelled_by_id
                    .map(|id| mc_core::Id::from(id).as_string())
                    .unwrap_or_default(),
                name: row.cancelled_by_name.clone().unwrap_or_default(),
            }),
            cancelled_by_comment_change: cancelled
                && context_str(row.context.as_ref(), "comment_change_cancelled_task_id") == task_id,
            auth_token: String::new(),
            remote_mcp_daemon_token: String::new(),
        }
    }
}

/// 从 `context` JSONB 里取一个字符串键（非字符串 / 缺键 → 空串）。
fn context_str(context: Option<&Value>, key: &str) -> String {
    context
        .and_then(|v| v.get(key))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// upstream `computeTaskKind`（`agent.go:1030`）。
#[must_use]
pub(crate) fn compute_task_kind(row: &TaskRow) -> String {
    if row.chat_session_id.is_some() {
        return "chat".into();
    }
    if row.autopilot_run_id.is_some() {
        return "autopilot".into();
    }
    if context_str(row.context.as_ref(), "type") == "quick_create" {
        return "quick_create".into();
    }
    if row.issue_id.is_none() {
        return "quick_create".into();
    }
    if row.trigger_comment_id.is_some() {
        return "comment".into();
    }
    "direct".into()
}

/// upstream `relativeWorkDir`（`agent.go:914`）：把绝对路径折成可展示的相对路径。
///
/// 三级回退：① 命中 `…/<workspace>/<taskDir>` 段就返回该后缀；② 命中常见 home 前缀
/// 就剥掉用户段；③ 否则取 basename。路径全部先规范化 `\` → `/`。
#[must_use]
pub(crate) fn relative_work_dir(work_dir: &str, workspace_id: &str, task_id: &str) -> String {
    if work_dir.is_empty() {
        return String::new();
    }
    let normalized = work_dir.replace('\\', "/");
    if !workspace_id.is_empty() && !task_id.is_empty() {
        let parts: Vec<&str> = normalized.split('/').collect();
        for i in 0..parts.len().saturating_sub(1) {
            if matches_workspace_segment(parts[i], workspace_id)
                && matches_task_segment(parts[i + 1], task_id)
            {
                return parts[i..].join("/");
            }
        }
    }
    if let Some(stripped) = strip_home_prefix(&normalized) {
        return stripped;
    }
    normalized
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .to_string()
}

/// upstream `taskDirSegment`：id 去掉 `-` 后取**末** 12 个十六进制字符。
fn task_dir_segment(id: &str) -> String {
    let s: String = id.chars().filter(|c| *c != '-').collect();
    if s.chars().count() > 12 {
        s.chars().skip(s.chars().count() - 12).collect()
    } else {
        s
    }
}

/// upstream `legacyTaskDirSegment`：id 去掉 `-` 后取**头** 8 个字符。
fn legacy_task_dir_segment(id: &str) -> String {
    let s: String = id.chars().filter(|c| *c != '-').collect();
    let take: usize = 8.min(s.chars().count());
    s.chars().take(take).collect::<String>().to_lowercase()
}

fn matches_workspace_segment(segment: &str, workspace_id: &str) -> bool {
    let lower = segment.to_lowercase();
    segment.eq_ignore_ascii_case(workspace_id)
        || lower.ends_with(&format!("-{}", legacy_task_dir_segment(workspace_id)))
        || lower.ends_with(&format!(
            "-{}",
            task_dir_segment(workspace_id).to_lowercase()
        ))
}

fn matches_task_segment(segment: &str, task_id: &str) -> bool {
    let lower = segment.to_lowercase();
    segment.eq_ignore_ascii_case(&legacy_task_dir_segment(task_id))
        || segment.eq_ignore_ascii_case(&task_dir_segment(task_id))
        || lower.ends_with(&format!("-{}", legacy_task_dir_segment(task_id)))
        || lower.ends_with(&format!("-{}", task_dir_segment(task_id).to_lowercase()))
}

/// upstream `stripHomePrefix`：`(?:[A-Za-z]:)?/(?:Users|home)/<user>[/<rest>]`。
///
/// 命中返回用户名段之后的余部（可能是空串），否则 `None`。
fn strip_home_prefix(normalized: &str) -> Option<String> {
    let (prefix_rest, _drive) = match normalized.as_bytes() {
        [drive, b':', rest @ ..] if drive.is_ascii_alphabetic() => {
            (std::str::from_utf8(rest).ok()?, Some(*drive))
        }
        _ => (normalized, None),
    };
    let rest = prefix_rest.strip_prefix('/')?;
    let mut segments = rest.splitn(3, '/');
    let root = segments.next()?;
    if !root.eq_ignore_ascii_case("Users") && !root.eq_ignore_ascii_case("home") {
        return None;
    }
    let user = segments.next()?;
    if user.is_empty() {
        return None;
    }
    Some(segments.next().unwrap_or_default().to_string())
}

/// 请求体形状不符 / 空 body → 400 `invalid request body`。
///
/// 刻意不用 `Json<T>` 提取器：它对类型不符回 **422**，而本仓（与上游
/// `json.NewDecoder().Decode`）都是 400。`null` body 等价于 Go 的「字段全缺」。
///
/// **只收对象**：serde 的 derive 会让「全字段带 `#[serde(default)]` 的结构体」能从
/// JSON **数组**反序列化（`[]` → 全默认），而 Go 的 `Decode` 对 `[]` 是
/// `cannot unmarshal array into Go value of type …` �⇒ 这里显式挡掉，与上游对齐。
pub(crate) fn decode_body<T: serde::de::DeserializeOwned + Default>(
    body: &axum::body::Bytes,
) -> Result<T, crate::error::ApiError> {
    let value: Value = serde_json::from_slice(body)
        .map_err(|_| super::scope::validation("invalid request body"))?;
    if value.is_null() {
        return Ok(T::default());
    }
    if !value.is_object() {
        return Err(super::scope::validation("invalid request body"));
    }
    serde_json::from_value(value).map_err(|_| super::scope::validation("invalid request body"))
}

/// NUL 清洗（upstream `util.SanitizeTextForPostgres`，GH #7098）：Postgres 的 TEXT
/// 不接受 U+0000，不洗净就会让整条写入报错。
#[must_use]
pub(crate) fn sanitize(value: &str) -> String {
    value.replace('\u{0}', "")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_work_dir_finds_workspace_and_task_segments() {
        // 上游 envRoot 形状是 `<workspaceSegment>/<taskSegment>/<name>`（**相邻**，
        // 中间没有 `tasks/` 这一层）—— 见 `agent_work_dir_test.go` 的前三例。
        let ws = "a05b0e10-ee7a-4603-a72d-a548b2390cb2";
        let task = "5c57b65b-ee7a-4603-a72d-b659c34a1dc3";
        let task_tail = &task.replace('-', "")[20..];
        assert_eq!(task_tail, "b659c34a1dc3");

        let path = format!("/Users/alice/multica_workspaces/{ws}/{task_tail}/workdir");
        assert_eq!(
            relative_work_dir(&path, ws, task),
            format!("{ws}/{task_tail}/workdir")
        );
        // 可读目录名：只靠**尾部**短 id 匹配，标签本身不算身份。
        let ws_tail = &ws.replace('-', "")[20..];
        let readable = format!(
            "/Users/alice/multica_workspaces/asset-feed-{ws_tail}/mul-6063-{task_tail}/workdir"
        );
        assert_eq!(
            relative_work_dir(&readable, ws, task),
            format!("asset-feed-{ws_tail}/mul-6063-{task_tail}/workdir")
        );
        // 遗留路径：`<slug>-<首8位>` / `<首8位>` 两种短 id 形状都能识别。
        assert_eq!(
            relative_work_dir(
                "/Users/alice/multica_workspaces/asset-feed-a05b0e10/mul-6063-5c57b65b/workdir",
                ws,
                task
            ),
            "asset-feed-a05b0e10/mul-6063-5c57b65b/workdir"
        );
        assert_eq!(
            relative_work_dir(
                &format!("/Users/alice/multica_workspaces/{ws}/5c57b65b/workdir"),
                ws,
                task
            ),
            format!("{ws}/5c57b65b/workdir")
        );
        // envRoot 段识别不出来时，退路是 home 前缀（绝不吐绝对路径）。
        assert_eq!(
            relative_work_dir("/Users/alice/multica_workspaces/other/x", ws, task),
            "multica_workspaces/other/x"
        );
    }

    #[test]
    fn relative_work_dir_strips_home_prefix() {
        assert_eq!(
            relative_work_dir("/Users/alice/code/x", "", ""),
            "code/x".to_string()
        );
        assert_eq!(
            relative_work_dir("C:\\Users\\alice\\code\\x", "", ""),
            "code/x".to_string()
        );
    }

    #[test]
    fn relative_work_dir_falls_back_to_basename() {
        assert_eq!(relative_work_dir("/opt/whatever", "", ""), "whatever");
        assert_eq!(relative_work_dir("", "ws", "task"), "");
    }

    #[test]
    fn strip_home_prefix_rejects_unrelated_roots() {
        assert_eq!(strip_home_prefix("/var/lib/x"), None);
        assert_eq!(strip_home_prefix("/Users"), None);
    }

    #[test]
    fn sanitize_removes_nul_only() {
        assert_eq!(sanitize("a\u{0}b\nc"), "ab\nc");
    }

    #[test]
    fn decode_body_accepts_null_as_default() {
        use axum::body::Bytes;
        let Ok(parsed) = decode_body::<RegisterRequest>(&Bytes::from_static(b"null")) else {
            panic!("null 应解码为默认值");
        };
        assert!(parsed.daemon_id.is_empty());
        // 形状不符（数组给结构体）与**空体**都是 400：Go 的 `Decode` 在空体上回
        // `io.EOF`，对 `[]` 回 `cannot unmarshal array into Go value of type …`，
        // 两者都落进 `invalid request body` 分支。
        assert!(decode_body::<RegisterRequest>(&Bytes::from_static(b"[]")).is_err());
        assert!(decode_body::<RegisterRequest>(&Bytes::from_static(b"[1]")).is_err());
        assert!(decode_body::<RegisterRequest>(&Bytes::from_static(b"true")).is_err());
        assert!(decode_body::<RegisterRequest>(&Bytes::from_static(b"")).is_err());
        // 缺字段的 `{}` 才是合法输入（全默认）。
        assert!(decode_body::<RegisterRequest>(&Bytes::from_static(b"{}")).is_ok());
    }
}
