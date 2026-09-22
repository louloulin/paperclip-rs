//! daemon 生命周期与心跳载荷 —— 上游 `server/pkg/protocol/messages.go` L118–L153、
//! L162–L167、L221–L232、L384–L445 冻结。
//!
//! 心跳是 WS 控制连接上**唯一**由 daemon 主动发起的非 RPC 帧（`hub.go:980` 的
//! `case protocol.EventDaemonHeartbeat`）；它的 ack 同时承担三件事：
//!
//! 1. server → daemon 的**协议协商**（`server_capabilities`）；
//! 2. HTTP 心跳响应的 WS 等价物（同形状，daemon 两条路都解同一个结构体）；
//! 3. 把「这个 runtime 已经被删了」从 HTTP `404` 搬成
//!    `status = runtime_gone` + `runtime_gone = true` 的**正常 ack**
//!    （`messages.go:401` 注释：撕连接会让死 UUID 一直心跳到进程重启）。

use serde::{Deserialize, Serialize};

use super::omit;

/// 心跳 ack 的 `status`：runtime 行已不存在（`messages.go:419`）。
pub const HEARTBEAT_STATUS_RUNTIME_GONE: &str = "runtime_gone";

/// daemon 连接后上报自己与它的 runtime 集合（`messages.go:221`）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DaemonRegisterPayload {
    /// 本机 daemon id。
    pub daemon_id: String,
    /// daemon 持有的 agent id。
    pub agent_id: String,
    /// 本机可用的 runtime 列表（上游无 `omitempty`）。
    pub runtimes: Vec<RuntimeInfo>,
}

/// 一条可用 runtime 的描述（`messages.go:228`）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RuntimeInfo {
    /// runtime 类型（如 `claude` / `codex`）。
    #[serde(rename = "type")]
    pub kind: String,
    /// 该 runtime 的 CLI 版本。
    pub version: String,
    /// 该 runtime 自己的状态串。
    pub status: String,
}

/// daemon → server 的心跳请求（`messages.go:384`）。
///
/// **与 `POST /api/daemon/heartbeat` 的 body 逐字同义**：两条传输共用一套语义，
/// 所以 daemon 侧只维护一个解码结构体。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DaemonHeartbeatRequestPayload {
    /// 本次心跳对应的单个 runtime。
    pub runtime_id: String,
    /// 该 daemon 支不支持批量导入（`omitempty`）；老 daemon 缺失即 `false`。
    #[serde(skip_serializing_if = "omit::boolean")]
    pub supports_batch_import: bool,
}

/// server → daemon 的心跳 ack（`messages.go:401`）。
///
/// 字段的**可选性**是这里的契约核心：
///
/// - `server_capabilities`：协议协商的**唯一**真值。daemon 不得从自己声明的能力
///   反推 server 支持了什么（`messages.go:401` 注释），所以缺失 = server 什么都没声明。
/// - 四个 `pending_*` 指针：缺失 = 这一轮没有该类待办。它们是**指针**而不是值，
///   因为 daemon 要区分「没有待办」与「有待办但字段全零」。
/// - `pending_local_skill_imports`（复数）是增量字段：认识它的 daemon 一次心跳处理多条
///   导入，不认识的老 daemon 按标准 JSON 行为忽略它、退回单数
///   `pending_local_skill_import`。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DaemonHeartbeatAckPayload {
    /// 回声的 runtime id。
    pub runtime_id: String,
    /// [`HEARTBEAT_STATUS_RUNTIME_GONE`] 或普通状态串。
    pub status: String,
    /// server 的能力声明（`omitempty`）。
    #[serde(skip_serializing_if = "omit::vec_is_empty")]
    pub server_capabilities: Vec<String>,
    /// 见结构体文档第 3 点；`omitempty`，缺失/false = 未消失。
    #[serde(skip_serializing_if = "omit::boolean")]
    pub runtime_gone: bool,
    /// 待执行的 CLI 升级（`omitempty`：无待办即 `null`/缺失）。
    #[serde(skip_serializing_if = "omit::option")]
    pub pending_update: Option<DaemonHeartbeatPendingUpdate>,
    /// 待枚举的模型列表请求。
    #[serde(skip_serializing_if = "omit::option")]
    pub pending_model_list: Option<DaemonHeartbeatPendingModelList>,
    /// 待枚举的本地技能清单。
    #[serde(skip_serializing_if = "omit::option")]
    pub pending_local_skills: Option<DaemonHeartbeatPendingLocalSkills>,
    /// 单数形态的本地技能导入（老 daemon 只认它）。
    #[serde(skip_serializing_if = "omit::option")]
    pub pending_local_skill_import: Option<DaemonHeartbeatPendingLocalSkillImport>,
    /// 复数形态的本地技能导入（新 daemon 并发处理）。
    #[serde(skip_serializing_if = "omit::vec_is_empty")]
    pub pending_local_skill_imports: Vec<DaemonHeartbeatPendingLocalSkillImport>,
}

/// 待执行的 CLI 升级（`messages.go:423`）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DaemonHeartbeatPendingUpdate {
    /// 升级请求 id（回报结果时用它关联）。
    pub id: String,
    /// 目标版本号。
    pub target_version: String,
}

/// 待枚举模型列表（`messages.go:430`）。只有 request id。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DaemonHeartbeatPendingModelList {
    /// 请求 id。
    pub id: String,
}

/// 待枚举本地技能清单（`messages.go:436`）。只有 request id。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DaemonHeartbeatPendingLocalSkills {
    /// 请求 id。
    pub id: String,
}

/// 待导入的本地技能（`messages.go:442`）。**只有单数形态带 `skill_key`。**
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DaemonHeartbeatPendingLocalSkillImport {
    /// 请求 id。
    pub id: String,
    /// 要导入的技能 key（发现路径上的稳定标识，不是名字）。
    pub skill_key: String,
}

/// 心跳携带的待办种类（`messages.go:151`–L153）。
///
/// **advisory only**：daemon 对每个 kind 的反应完全一样（立刻补一次心跳，把排队的
/// 东西领走），所以新 server 发来的未知 kind 在老 daemon 上依然安全 —— 这也是为什么
/// 这里只给常量与谓词，而不做会拒绝未知值的枚举。
pub mod pending_work_kind {
    /// 模型列表请求。
    pub const MODEL_LIST: &str = "model_list";
    /// 本地技能清单请求。
    pub const LOCAL_SKILLS: &str = "local_skills";
    /// 本地技能导入请求。
    pub const LOCAL_SKILL_IMPORT: &str = "local_skill_import";

    /// 已知 kind 全表（`messages.go:151`–L153）。
    pub const KNOWN: [&str; 3] = [MODEL_LIST, LOCAL_SKILLS, LOCAL_SKILL_IMPORT];

    /// 已知 kind 判定。未知值是**合法**输入，只是不带额外行为（见模块文档）。
    #[must_use]
    pub fn is_known(kind: &str) -> bool {
        KNOWN.contains(&kind)
    }
}

/// server → daemon 的待办唤醒提示（`messages.go:162`）。
///
/// 它不携带任何工作：daemon 只是据此**立刻**补一次 [`DaemonHeartbeatRequestPayload`]，
/// 真正的工作仍走正常心跳领取。所以丢失、重复、被忽略都是安全的
/// （`messages.go:156`、`events.go:160` 注释；MUL-5444）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PendingWorkPayload {
    /// 该提示针对的 runtime。
    pub runtime_id: String,
    /// [`pending_work_kind`] 之一（`omitempty`）。
    #[serde(skip_serializing_if = "omit::string")]
    pub kind: String,
}

/// 运行时 profile 变更唤醒提示（`messages.go:136`）。
///
/// 同样是「去拉一遍」而非「数据在此」：daemon 收到后走既有 HTTP 端点取 profile 并注册
/// runtime。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RuntimeProfilesChangedPayload {
    /// 发生变更的 workspace。
    pub workspace_id: String,
    /// 具体变更的 profile（`omitempty`：删除整批时可能没有单个 id）。
    #[serde(skip_serializing_if = "omit::string")]
    pub runtime_profile_id: String,
}

/// 账号级「workspace 成员集变了」提示（`messages.go:144`）：`struct{}`，**不带数据**，
/// server 仍是权威。Rust 侧是空结构体，序列化成 `{}`（不是 `null`），与 Go 一致。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WorkspacesChangedPayload {}
