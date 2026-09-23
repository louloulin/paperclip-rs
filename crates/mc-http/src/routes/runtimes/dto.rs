//! runtime 切片的线上格式：响应 DTO（逐字段对齐上游 `runtime.go` / `runtime_profile.go`
//! 的 `*Response`）与请求体（`*Request`）。
//!
//! 时间戳统一走 [`super::access::timestamp`]（RFC3339 / 秒精度 / UTC `Z`）。
//! `null` 与缺省在 serde 里都是 `None`，与上游 Go 的指针语义一致 —— 因此
//! `description: null` 是「不改」而不是「清空」（上游 `*string` 分辨不出两者）。

use mc_core::Id;
use mc_repos::runtime::{
    ActivityRow, AgentRuntimeRow, RuntimeProfileRow, RuntimeUsageByAgentRow, RuntimeUsageByHourRow,
    RuntimeUsageRow,
};
use serde::{Deserialize, Serialize};

use super::access::{timestamp, timestamp_opt};
use super::protocol::{launch_header, profile_runtime_type};

// ---------------------------------------------------------------------------
// 响应
// ---------------------------------------------------------------------------

/// upstream `RuntimeProfileResponse`。
#[derive(Debug, Clone, Serialize)]
pub(crate) struct RuntimeProfileDto {
    pub(crate) id: String,
    pub(crate) workspace_id: String,
    pub(crate) display_name: String,
    pub(crate) protocol_family: String,
    pub(crate) runtime_type: String,
    pub(crate) command_name: String,
    pub(crate) description: Option<String>,
    pub(crate) fixed_args: Vec<String>,
    pub(crate) visibility: String,
    pub(crate) created_by: Option<String>,
    pub(crate) enabled: bool,
    pub(crate) created_at: String,
    pub(crate) updated_at: String,
}

impl RuntimeProfileDto {
    pub(crate) fn from_row(row: &RuntimeProfileRow) -> Self {
        Self {
            id: row.id.as_string(),
            workspace_id: row.workspace_id.as_string(),
            display_name: row.display_name.clone(),
            protocol_family: row.protocol_family.clone(),
            // 老行 `runtime_type` 是空串 → 回退到 protocol_family（上游 `ProfileRuntimeType`）。
            runtime_type: profile_runtime_type(&row.runtime_type, &row.protocol_family),
            command_name: row.command_name.clone(),
            description: row.description.clone(),
            fixed_args: fixed_args(&row.fixed_args),
            visibility: row.visibility.clone(),
            created_by: row.created_by.map(Id::as_string),
            enabled: row.enabled,
            created_at: timestamp(row.created_at),
            updated_at: timestamp(row.updated_at),
        }
    }
}

/// `fixed_args` JSONB → `Vec<String>`；非数组/非字符串项忽略（上游 `json.Unmarshal`
/// 失败时留下空数组）。
fn fixed_args(value: &serde_json::Value) -> Vec<String> {
    match value {
        serde_json::Value::Array(items) => items
            .iter()
            .filter_map(|item| item.as_str().map(ToString::to_string))
            .collect(),
        _ => Vec::new(),
    }
}

/// upstream `AgentRuntimeResponse`。
#[derive(Debug, Clone, Serialize)]
pub(crate) struct AgentRuntimeDto {
    pub(crate) id: String,
    pub(crate) workspace_id: String,
    pub(crate) daemon_id: Option<String>,
    pub(crate) name: String,
    pub(crate) custom_name: Option<String>,
    pub(crate) runtime_mode: String,
    pub(crate) provider: String,
    pub(crate) launch_header: String,
    pub(crate) status: String,
    pub(crate) device_info: String,
    pub(crate) metadata: serde_json::Value,
    pub(crate) owner_id: Option<String>,
    pub(crate) visibility: String,
    pub(crate) profile_id: Option<String>,
    pub(crate) last_seen_at: Option<String>,
    pub(crate) created_at: String,
    pub(crate) updated_at: String,
}

impl AgentRuntimeDto {
    pub(crate) fn from_row(row: &AgentRuntimeRow) -> Self {
        Self {
            id: row.id.as_string(),
            workspace_id: row.workspace_id.as_string(),
            daemon_id: row.daemon_id.clone(),
            name: row.name.clone(),
            custom_name: row.custom_name.clone(),
            runtime_mode: row.runtime_mode.clone(),
            provider: row.provider.clone(),
            launch_header: launch_header(&row.provider),
            status: row.status.clone(),
            device_info: row.device_info.clone(),
            metadata: row.metadata.clone(),
            owner_id: row.owner_id.map(Id::as_string),
            visibility: row.visibility.clone(),
            profile_id: row.profile_id.map(Id::as_string),
            last_seen_at: timestamp_opt(row.last_seen_at),
            created_at: timestamp(row.created_at),
            updated_at: timestamp(row.updated_at),
        }
    }
}

/// upstream `RuntimeUsageResponse`（带 `runtime_id`）。
#[derive(Debug, Clone, Serialize)]
pub(crate) struct RuntimeUsageDto {
    pub(crate) runtime_id: String,
    pub(crate) date: String,
    pub(crate) provider: String,
    pub(crate) model: String,
    pub(crate) input_tokens: i64,
    pub(crate) output_tokens: i64,
    pub(crate) cache_read_tokens: i64,
    pub(crate) cache_write_tokens: i64,
    pub(crate) cost_usd_ticks: i64,
    pub(crate) uncosted_input_tokens: i64,
    pub(crate) uncosted_output_tokens: i64,
    pub(crate) uncosted_cache_read_tokens: i64,
    pub(crate) uncosted_cache_write_tokens: i64,
}

impl RuntimeUsageDto {
    pub(crate) fn from_row(runtime_id: Id, row: &RuntimeUsageRow) -> Self {
        Self {
            runtime_id: runtime_id.as_string(),
            // 上游 `row.Date.Time.Format("2006-01-02")`。
            date: row.date.to_string(),
            provider: row.provider.clone(),
            model: row.model.clone(),
            input_tokens: row.input_tokens,
            output_tokens: row.output_tokens,
            cache_read_tokens: row.cache_read_tokens,
            cache_write_tokens: row.cache_write_tokens,
            cost_usd_ticks: row.cost_usd_ticks,
            uncosted_input_tokens: row.uncosted_input_tokens,
            uncosted_output_tokens: row.uncosted_output_tokens,
            uncosted_cache_read_tokens: row.uncosted_cache_read_tokens,
            uncosted_cache_write_tokens: row.uncosted_cache_write_tokens,
        }
    }
}

/// upstream `RuntimeUsageByAgentResponse`（**没有** `runtime_id`）。
#[derive(Debug, Clone, Serialize)]
pub(crate) struct RuntimeUsageByAgentDto {
    pub(crate) agent_id: String,
    pub(crate) provider: String,
    pub(crate) model: String,
    pub(crate) input_tokens: i64,
    pub(crate) output_tokens: i64,
    pub(crate) cache_read_tokens: i64,
    pub(crate) cache_write_tokens: i64,
    pub(crate) cost_usd_ticks: i64,
    pub(crate) uncosted_input_tokens: i64,
    pub(crate) uncosted_output_tokens: i64,
    pub(crate) uncosted_cache_read_tokens: i64,
    pub(crate) uncosted_cache_write_tokens: i64,
    pub(crate) task_count: i64,
}

impl From<&RuntimeUsageByAgentRow> for RuntimeUsageByAgentDto {
    fn from(row: &RuntimeUsageByAgentRow) -> Self {
        Self {
            agent_id: row.agent_id.as_string(),
            provider: row.provider.clone(),
            model: row.model.clone(),
            input_tokens: row.input_tokens,
            output_tokens: row.output_tokens,
            cache_read_tokens: row.cache_read_tokens,
            cache_write_tokens: row.cache_write_tokens,
            cost_usd_ticks: row.cost_usd_ticks,
            uncosted_input_tokens: row.uncosted_input_tokens,
            uncosted_output_tokens: row.uncosted_output_tokens,
            uncosted_cache_read_tokens: row.uncosted_cache_read_tokens,
            uncosted_cache_write_tokens: row.uncosted_cache_write_tokens,
            task_count: row.task_count,
        }
    }
}

/// upstream `RuntimeUsageByHourResponse`：零活动的桶不返回，客户端补 0..23。
#[derive(Debug, Clone, Serialize)]
pub(crate) struct RuntimeUsageByHourDto {
    pub(crate) hour: i64,
    pub(crate) model: String,
    pub(crate) input_tokens: i64,
    pub(crate) output_tokens: i64,
    pub(crate) cache_read_tokens: i64,
    pub(crate) cache_write_tokens: i64,
    pub(crate) cost_usd_ticks: i64,
    pub(crate) uncosted_input_tokens: i64,
    pub(crate) uncosted_output_tokens: i64,
    pub(crate) uncosted_cache_read_tokens: i64,
    pub(crate) uncosted_cache_write_tokens: i64,
    pub(crate) task_count: i64,
}

impl From<&RuntimeUsageByHourRow> for RuntimeUsageByHourDto {
    fn from(row: &RuntimeUsageByHourRow) -> Self {
        Self {
            hour: row.hour,
            model: row.model.clone(),
            input_tokens: row.input_tokens,
            output_tokens: row.output_tokens,
            cache_read_tokens: row.cache_read_tokens,
            cache_write_tokens: row.cache_write_tokens,
            cost_usd_ticks: row.cost_usd_ticks,
            uncosted_input_tokens: row.uncosted_input_tokens,
            uncosted_output_tokens: row.uncosted_output_tokens,
            uncosted_cache_read_tokens: row.uncosted_cache_read_tokens,
            uncosted_cache_write_tokens: row.uncosted_cache_write_tokens,
            task_count: row.task_count,
        }
    }
}

/// upstream 的匿名 `HourlyActivity`（`activity` 端点）。
#[derive(Debug, Clone, Serialize)]
pub(crate) struct ActivityDto {
    pub(crate) hour: i64,
    pub(crate) count: i64,
}

impl From<&ActivityRow> for ActivityDto {
    fn from(row: &ActivityRow) -> Self {
        Self {
            hour: row.hour,
            count: row.count,
        }
    }
}

// ---------------------------------------------------------------------------
// 请求体
// ---------------------------------------------------------------------------

/// upstream `createRuntimeProfileRequest`。缺字段等价于 Go 的零值 ⇒
/// `display_name: ""` → 400 `"display_name is required"`。
#[derive(Debug, Default, Deserialize)]
pub(crate) struct CreateProfileRequest {
    #[serde(default)]
    pub(crate) display_name: String,
    #[serde(default)]
    pub(crate) protocol_family: String,
    #[serde(default)]
    pub(crate) runtime_type: String,
    #[serde(default)]
    pub(crate) command_name: String,
    #[serde(default)]
    pub(crate) description: Option<String>,
    #[serde(default)]
    pub(crate) fixed_args: Vec<String>,
    #[serde(default)]
    pub(crate) enabled: Option<bool>,
}

/// upstream `updateRuntimeProfileRequest`。
///
/// `runtime_type` / `protocol_family` 存在即 400（不可变）；`description` 是
/// 「缺省或 `null` = 不动，空串 = 置空串」（与上游 `*string` + `ptrToText` 逐字一致）。
#[derive(Debug, Default, Deserialize)]
pub(crate) struct UpdateProfileRequest {
    #[serde(default)]
    pub(crate) runtime_type: Option<String>,
    #[serde(default)]
    pub(crate) protocol_family: Option<String>,
    #[serde(default)]
    pub(crate) display_name: Option<String>,
    #[serde(default)]
    pub(crate) command_name: Option<String>,
    #[serde(default)]
    pub(crate) description: Option<String>,
    #[serde(default)]
    pub(crate) fixed_args: Option<Vec<String>>,
    #[serde(default)]
    pub(crate) enabled: Option<bool>,
}

/// upstream `UpdateAgentRuntimeRequest`。
#[derive(Debug, Default, Deserialize)]
pub(crate) struct UpdateRuntimeRequest {
    #[serde(default)]
    pub(crate) visibility: Option<String>,
    #[serde(default)]
    pub(crate) custom_name: Option<String>,
    #[serde(default)]
    pub(crate) apply_to_machine: bool,
}

/// upstream `unbindAgentsAndDeleteRuntimeRequest`。
#[derive(Debug, Default, Deserialize)]
pub(crate) struct UnbindAgentsAndDeleteRequest {
    #[serde(default)]
    pub(crate) expected_active_agent_ids: Vec<String>,
}
