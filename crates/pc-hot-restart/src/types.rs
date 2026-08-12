#![forbid(unsafe_code)]
//! Hot-restart 的跨进程 JSON 契约，与 Node 服务字段保持 camelCase。

use serde::{Deserialize, Serialize};

/// 旧 server 退出时使用的信号。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShutdownSignal {
    /// Ctrl-C。
    SigInt,
    /// SIGTERM。
    SigTerm,
}

impl ShutdownSignal {
    /// 返回 Node 兼容的字符串。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SigInt => "SIGINT",
            Self::SigTerm => "SIGTERM",
        }
    }
}

impl Serialize for ShutdownSignal {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ShutdownSignal {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        match String::deserialize(deserializer)?.as_str() {
            "SIGINT" => Ok(Self::SigInt),
            "SIGTERM" => Ok(Self::SigTerm),
            value => Err(serde::de::Error::custom(format!("invalid shutdown signal {value}"))),
        }
    }
}

/// shutdown snapshot 中的运行记录。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HotRestartIntentRun {
    /// heartbeat run id。
    #[serde(rename = "runId")]
    pub run_id: String,
    /// company id。
    #[serde(rename = "companyId")]
    pub company_id: String,
    /// agent id。
    #[serde(rename = "agentId")]
    pub agent_id: String,
    /// adapter 类型。
    #[serde(rename = "adapterType")]
    pub adapter_type: String,
    /// 记录时的运行状态。
    pub status: String,
    /// adapter 进程 PID。
    #[serde(rename = "processPid")]
    pub process_pid: Option<i32>,
    /// adapter 进程组 ID。
    #[serde(rename = "processGroupId")]
    pub process_group_id: Option<i32>,
    /// 关联 issue id。
    #[serde(rename = "issueId")]
    pub issue_id: Option<String>,
}

/// 旧 server 退出前写入的快照。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShutdownSnapshot {
    /// 快照时间。
    #[serde(rename = "capturedAt")]
    pub captured_at: String,
    /// 退出信号。
    pub signal: ShutdownSignal,
    /// 快照时的 active runs。
    #[serde(rename = "activeRuns")]
    pub active_runs: Vec<HotRestartIntentRun>,
}

/// 一个 run 在 reconcile 阶段的分类结果（与 Node HotRestartReportRun.classification 1:1）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotRestartRunClassification {
    Adopted,
    FinalizedWhileDown,
    Lost,
    Skipped,
}

impl HotRestartRunClassification {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Adopted => "adopted",
            Self::FinalizedWhileDown => "finalized_while_down",
            Self::Lost => "lost",
            Self::Skipped => "skipped",
        }
    }
}

impl Serialize for HotRestartRunClassification {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for HotRestartRunClassification {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        match String::deserialize(deserializer)?.as_str() {
            "adopted" => Ok(Self::Adopted),
            "finalized_while_down" => Ok(Self::FinalizedWhileDown),
            "lost" => Ok(Self::Lost),
            "skipped" => Ok(Self::Skipped),
            other => Err(serde::de::Error::custom(format!(
                "invalid hot-restart run classification {other}"
            ))),
        }
    }
}

/// hot-restart-intent.json 的版本 1。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HotRestartIntent {
    /// 协议版本。
    pub version: u8,
    /// 请求时间。
    #[serde(rename = "requestedAt")]
    pub requested_at: String,
    /// 被替换的旧 server PID。
    #[serde(rename = "previousServerPid")]
    pub previous_server_pid: i32,
    /// server boot identity。
    #[serde(rename = "previousServerIdentity", skip_serializing_if = "Option::is_none")]
    pub previous_server_identity: Option<String>,
    /// 操作系统记录的启动时间。
    #[serde(rename = "previousServerStartedAt", skip_serializing_if = "Option::is_none")]
    pub previous_server_started_at: Option<String>,
    /// 旧 server 版本。
    #[serde(rename = "previousServerVersion")]
    pub previous_server_version: Option<String>,
    /// 是否要求 drain。
    #[serde(rename = "drainRequired")]
    pub drain_required: bool,
    /// 发起请求的 run id。
    #[serde(rename = "requestedByRunId")]
    pub requested_by_run_id: Option<String>,
    /// 预检出的 active run ids。
    #[serde(rename = "preflightActiveRunIds")]
    pub preflight_active_run_ids: Vec<String>,
    /// 退出前快照。
    #[serde(rename = "shutdownSnapshot", skip_serializing_if = "Option::is_none")]
    pub shutdown_snapshot: Option<ShutdownSnapshot>,
}

/// 报告中单个 run 的处理记录。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HotRestartReportRun {
    /// snapshot 运行信息。
    #[serde(flatten)]
    pub run: HotRestartIntentRun,
    /// adopted/finalized_while_down/lost/skipped。
    pub classification: String,
    /// 分类原因。
    pub reason: String,
}

/// 新 server 完成交接后的报告。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HotRestartReport {
    /// 协议版本。
    pub version: u8,
    /// 原始请求时间。
    #[serde(rename = "requestedAt")]
    pub requested_at: String,
    /// 完成时间。
    #[serde(rename = "completedAt")]
    pub completed_at: String,
    /// drain 标记。
    #[serde(rename = "drainRequired")]
    pub drain_required: bool,
    /// 旧 server PID。
    #[serde(rename = "previousServerPid")]
    pub previous_server_pid: i32,
    /// 新 server PID。
    #[serde(rename = "newServerPid")]
    pub new_server_pid: i32,
    /// 旧版本。
    #[serde(rename = "previousServerVersion")]
    pub previous_server_version: Option<String>,
    /// 新版本。
    #[serde(rename = "newServerVersion")]
    pub new_server_version: String,
    /// 成功接管的 ids。
    #[serde(rename = "adoptedRunIds")]
    pub adopted_run_ids: Vec<String>,
    /// 停机期间完成的 ids。
    #[serde(rename = "finalizedWhileDownRunIds")]
    pub finalized_while_down_run_ids: Vec<String>,
    /// 丢失的 ids。
    #[serde(rename = "lostRunIds")]
    pub lost_run_ids: Vec<String>,
    /// 跳过的 ids。
    #[serde(rename = "skippedRunIds")]
    pub skipped_run_ids: Vec<String>,
    /// 逐 run 详情。
    pub runs: Vec<HotRestartReportRun>,
}
