//! Plugin manifest 中 job declaration 的输入类型。
//!
//! 与 Node `PluginJobDeclaration` 1:1 对齐。

use serde::{Deserialize, Serialize};

/// Manifest 中声明的一个 scheduled job。
///
/// 与 Node `@paperclipai/shared` `PluginJobDeclaration` 1:1 对齐：
/// - `job_key` —— 在 plugin 内稳定且唯一的标识符
/// - `display_name` —— 展示名
/// - `description` —— 可选描述
/// - `schedule` —— 可选 cron 表达式（如 `"0 * * * *"`）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginJobDeclaration {
    pub job_key: String,
    pub display_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule: Option<String>,
}

impl PluginJobDeclaration {
    /// 提供一个最小构造器（测试用）。
    pub fn new(job_key: impl Into<String>, display_name: impl Into<String>) -> Self {
        Self {
            job_key: job_key.into(),
            display_name: display_name.into(),
            description: None,
            schedule: None,
        }
    }

    /// 取出 cron schedule（缺失则空字符串，与 Node 行为一致）。
    pub fn schedule_or_empty(&self) -> &str {
        self.schedule.as_deref().unwrap_or("")
    }
}

/// 创建 job run 的输入（与 Node `CreateJobRunInput` 1:1 对齐）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateJobRunInput {
    pub job_id: String,
    pub plugin_id: String,
    pub trigger: JobRunTrigger,
}

/// 完成 job run 的输入（与 Node `CompleteJobRunInput` 1:1 对齐）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompleteJobRunInput {
    pub status: JobRunStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<i32>,
}

use crate::types::{JobRunStatus, JobRunTrigger};
