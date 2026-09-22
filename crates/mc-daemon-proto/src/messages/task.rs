//! 任务面载荷 —— 上游 `server/pkg/protocol/messages.go` L118–L135、L168–L184、
//! L201–L219 冻结。
//!
//! 这里的 5 个结构体分成两组，**不要混用**：
//!
//! - **广播载荷**（[`TaskProgressPayload`]、[`TaskCompletedPayload`]、
//!   [`TaskMessagePayload`]）：server → 前端 WS 广播（事件
//!   [`crate::events::TASK_PROGRESS`] / [`crate::events::TASK_COMPLETED`] /
//!   [`crate::events::TASK_MESSAGE`]），并同步给 daemon 观察。
//! - **daemon 定向提示**（[`TaskAvailablePayload`]）：只给某台机器的唤醒提示，
//!   与「哪个任务在跑」无关。
//!
//! [`TaskMessagePayload`] 不是 HTTP 上报体 `TaskMessageRequest` 的复制品：HTTP 那个多出
//! `input`/`output_truncated`/`created_at` 的服务端语义（落库时 `NULL` vs false 的区分），
//! 线上广播体只带前端渲染需要的字段。两者的 `output_truncated` **都是三态**
//! （[`TaskMessagePayload::output_truncated`]）：缺失 = 从来没有 daemon 量过它，
//! 客户端必须按「未知」渲染，而不是「完整」。
//!
//! 任务面的状态迁移真值在 `events.go` L34–L42（本 crate 把它们逐条保留在
//! [`crate::events`] 对应常量的文档注释里）；状态机本身由 M3-3 实现。

use serde::{Deserialize, Serialize};

use super::omit;

/// server → daemon：这个任务分配给你（`messages.go:118`）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TaskDispatchPayload {
    /// 任务 id。
    pub task_id: String,
    /// 来源 issue id。
    pub issue_id: String,
    /// issue 标题。
    pub title: String,
    /// issue 描述。
    pub description: String,
}

/// server → daemon：**唤醒提示**，不是任务本体（`messages.go:127`）。
///
/// daemon 收到后仍通过既有 HTTP claim 端点领活；因此 `task_id` 只做提示
/// （`omitempty`：批量唤醒时可能不指名具体任务）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TaskAvailablePayload {
    /// 有活可领的 runtime。
    pub runtime_id: String,
    /// 可选的提示任务 id。
    #[serde(skip_serializing_if = "omit::string")]
    pub task_id: String,
}

/// daemon → server → 前端：执行进度（`messages.go:168`）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TaskProgressPayload {
    /// 任务 id。
    pub task_id: String,
    /// 一行进度摘要。
    pub summary: String,
    /// 当前步（`omitempty`：0 = 未提供）。
    #[serde(skip_serializing_if = "omit::i32")]
    pub step: i32,
    /// 总步数（`omitempty`：0 = 未知）。
    #[serde(skip_serializing_if = "omit::i32")]
    pub total: i32,
}

/// daemon → server：任务完成（`messages.go:176`）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TaskCompletedPayload {
    /// 任务 id。
    pub task_id: String,
    /// 产出的 PR 链接（`omitempty`）。
    #[serde(skip_serializing_if = "omit::string")]
    pub pr_url: String,
    /// 执行输出（`omitempty`）。
    #[serde(skip_serializing_if = "omit::string")]
    pub output: String,
}

/// 单条 agent 执行消息（工具调用 / 文本 / 错误）的**广播**形态（`messages.go:201`）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TaskMessagePayload {
    /// 一次后端执行内不透明的工具调用标识（`omitempty`）。
    #[serde(skip_serializing_if = "omit::string")]
    pub call_id: String,
    /// 任务 id。
    pub task_id: String,
    /// 来源 issue id（`omitempty`）。
    #[serde(skip_serializing_if = "omit::string")]
    pub issue_id: String,
    /// 执行内序号。
    pub seq: i32,
    /// 消息种类：`text` / `tool_use` / `tool_result` / `error`（线上键名 `"type"`）。
    #[serde(rename = "type")]
    pub kind: String,
    /// 工具名（`tool_use` / `tool_result`，`omitempty`）。
    #[serde(skip_serializing_if = "omit::string")]
    pub tool: String,
    /// 文本内容（`omitempty`）。
    #[serde(skip_serializing_if = "omit::string")]
    pub content: String,
    /// 工具入参（`tool_use`，`omitempty`）。Go 的 `nil` 与空 map 在 `omitempty` 下等价，
    /// 所以直接用 map 而不是 `Option`。
    #[serde(skip_serializing_if = "omit::map")]
    pub input: serde_json::Map<String, serde_json::Value>,
    /// 工具输出（`tool_result`，`omitempty`）。
    #[serde(skip_serializing_if = "omit::string")]
    pub output: String,
    /// **三态**：`None` = 无任何 daemon 量过（历史行 / 老 daemon），`Some(false)` = 完整，
    /// `Some(true)` = 被截断。`None` 必须一路保持 `None`，绝不能在解码时折成 `false`。
    #[serde(skip_serializing_if = "omit::option")]
    pub output_truncated: Option<bool>,
    /// 消息时间（`omitempty`：缺失时落库用数据库时间兜底）。
    #[serde(skip_serializing_if = "omit::string")]
    pub created_at: String,
}
