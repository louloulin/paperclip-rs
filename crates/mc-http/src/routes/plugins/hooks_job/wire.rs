//! 出站请求体的 wire 结构（字段名 = 上游 `hookRequestBody` 逐字）+ 两套触发器的折点。
//!
//! 从 `hooks_job.rs` 拆出来是门 ⑩ 的 800 行硬上限。

use super::{DateTime, HookTrigger, Map, Utc, Value};

// ---------------------------------------------------------------------------
// 出站请求体的 wire 结构（字段名 = 上游 `hookRequestBody` 逐字）
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct HookBody {
    pub(crate) version: i32,
    pub(crate) invocation_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) delivery_id: Option<String>,
    pub(crate) attempt: i32,
    pub(crate) occurred_at: DateTime<Utc>,
    pub(crate) hook_key: String,
    pub(crate) trigger: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) event_type: Option<String>,
    pub(crate) workspace_id: String,
    pub(crate) installation_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) issue_id: Option<String>,
    pub(crate) actor: HookBodyActor,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) input: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) config: Option<Map<String, Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) callback_token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) callback_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) schedule: Option<HookBodySchedule>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct HookBodyActor {
    #[serde(rename = "type")]
    pub(crate) kind: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub(crate) id: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct HookBodySchedule {
    pub(crate) planned_at: DateTime<Utc>,
}

/// `HookTrigger` → `mc_core::plugin::PluginInvocationTrigger`（回调令牌的 grant 字段）。
///
/// 两套字面量一一对应（`ui`/`manual`/`event`/`agent`/`schedule`），本文件是唯一的折点。
pub(super) fn invocation_trigger(trigger: HookTrigger) -> mc_core::plugin::PluginInvocationTrigger {
    match trigger {
        HookTrigger::Ui => mc_core::plugin::PluginInvocationTrigger::Ui,
        HookTrigger::Manual => mc_core::plugin::PluginInvocationTrigger::Manual,
        HookTrigger::Event => mc_core::plugin::PluginInvocationTrigger::Event,
        HookTrigger::Agent => mc_core::plugin::PluginInvocationTrigger::Agent,
        HookTrigger::Schedule => mc_core::plugin::PluginInvocationTrigger::Schedule,
    }
}
