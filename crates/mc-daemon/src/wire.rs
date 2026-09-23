//! daemon → server 的 HTTP 面线上类型（M3-7 / LUM-1438）。
//!
//! ## 为什么和 `mc-http` 的 `routes/daemon/dto.rs` 有两份定义
//!
//! 那一边是 `pub(crate) struct`（服务端**入站**方向：`Deserialize`），这一边是
//! 客户端**出站**方向（`Serialize`）。两者必须逐字同形，但它们在不同的 crate、
//! 不同的可见性级别上 —— 合并它们要么把服务端 DTO 提升成公开 API，要么让
//! `mc-daemon` 依赖 `mc-http`（后者是真正的错依赖：daemon 二进制不该链接服务端路由）。
//!
//! 因此这里刻意重复一份，**字段名逐字对齐上游 Go 的 `json:"..."` 标签**，每个类型
//! 都标注上游出处；两侧漂移由 `docs/32-M3-DAEMON-FACE.md` 的 D-9 与
//! `crates/mc-daemon/tests/client_loop.rs` 的往返用例兜住。
//!
//! ## 路径常量
//!
//! 路径写在这里而不是散在 [`crate::client`] 里：它们是**线路契约**的一部分，
//! 与 `crates/mc-http/src/routes/daemon/mod.rs` 的 `.route(...)` 表一一对应
//! （路由侧由门 ⑦ route-parity 守着）。

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// `POST /api/daemon/register`。
pub const REGISTER_PATH: &str = "/api/daemon/register";
/// `POST /api/daemon/deregister`。
pub const DEREGISTER_PATH: &str = "/api/daemon/deregister";
/// `POST /api/daemon/heartbeat`。
pub const HEARTBEAT_PATH: &str = "/api/daemon/heartbeat";
/// `POST /api/daemon/tasks/claim`（等价路径 `/api/daemon/claim`，服务端同一个 handler）。
pub const CLAIM_PATH: &str = "/api/daemon/tasks/claim";

/// upstream `DaemonRegisterRequest`（`daemon.go:196`）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegisterRequest {
    /// 目标 workspace（字符串 UUID）。
    #[serde(default)]
    pub workspace_id: String,
    /// 本机 daemon id。
    #[serde(default)]
    pub daemon_id: String,
    /// 历史 hostname 派生的 daemon id（迁移用）。
    #[serde(default)]
    pub legacy_daemon_ids: Vec<String>,
    /// 机器名。
    #[serde(default)]
    pub device_name: String,
    /// multica CLI 版本。
    #[serde(default)]
    pub cli_version: String,
    /// `"desktop"` 表示由 Electron 应用拉起。
    #[serde(default)]
    pub launched_by: String,
    /// 本机可用的 runtime 列表。
    #[serde(default)]
    pub runtimes: Vec<RegisterRuntime>,
    /// 解析失败的自定义 profile。
    #[serde(default)]
    pub failed_profiles: Vec<FailedProfile>,
}

/// `register.runtimes[]` 的一项（上游匿名结构体，`daemon.go:207`）。
///
/// `kind` 在线上叫 **`type`** —— 上游字段名就是 `type`（protocol family，
/// 不是 provider name），serde 侧用 `rename` 对齐。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegisterRuntime {
    /// 展示名。
    #[serde(default)]
    pub name: String,
    /// protocol family。
    #[serde(default, rename = "type")]
    pub kind: String,
    /// 该 CLI 自己的版本。
    #[serde(default)]
    pub version: String,
    /// daemon 自报状态（只有 `"offline"` 有特殊含义）。
    #[serde(default)]
    pub status: String,
    /// 非空 = 这是某个自定义 runtime profile 的实例。
    #[serde(default)]
    pub profile_id: String,
}

/// `register.failed_profiles[]` 的一项（上游匿名结构体，`daemon.go:218`）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FailedProfile {
    /// profile id。
    #[serde(default)]
    pub profile_id: String,
    /// 解析到的命令名。
    #[serde(default)]
    pub command_name: String,
    /// 失败原因。
    #[serde(default)]
    pub reason: String,
}

/// `POST /api/daemon/register` 的响应体。
///
/// 服务端回 `{runtimes, repos, repos_version, settings}`（`lifecycle.rs` 的 `register`
/// 尾部），其中 `runtimes[]` 是 `AgentRuntimeResponse` 投影。这里只取客户端真正用得到的
/// 那几个字段，其余原样落进 [`Self::extra`] 之外被忽略 —— 反序列化忽略未知字段是
/// 刻意选择：服务端加字段不该让旧 daemon 直接报错。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct RegisterResponse {
    /// 服务端落库后的 runtime 台账（新登记 + 已存在的老行）。
    #[serde(default)]
    pub runtimes: Vec<RegisteredRuntime>,
    /// 服务端当前 workspace 的 repos 快照（原样透传，客户端只用版本号）。
    #[serde(default)]
    pub repos: Value,
    /// repos 版本号（daemon 侧用它判断是否要重新拉 clone 清单）。
    #[serde(default)]
    pub repos_version: i64,
    /// workspace 设置（原样透传）。
    #[serde(default)]
    pub settings: Value,
}

/// `AgentRuntimeResponse` 里客户端用得到的子集（`runtime.go:25`）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct RegisteredRuntime {
    /// runtime id（字符串 UUID）。
    #[serde(default)]
    pub id: String,
    /// workspace id。
    #[serde(default)]
    pub workspace_id: String,
    /// 服务端看到的绑定机器；未绑定时为 `null`。
    #[serde(default)]
    pub daemon_id: Option<String>,
    /// 服务端口径的展示名（可能被 `custom_name` 覆盖）。
    #[serde(default)]
    pub name: String,
    /// protocol family。
    #[serde(default)]
    pub provider: String,
    /// `online` / `offline` / …（服务端口径）。
    #[serde(default)]
    pub status: String,
}

/// upstream `DaemonDeregisterRequest`（`daemon.go:939` 附近）。
///
/// `offline_reasons` 是**按请求原文 id 索引的对象**（上游 `map[string]json.RawMessage`），
/// 不是数组 —— 值形状不做约束，服务端原样透传给 `agent_runtime.metadata.offline_reason`。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeregisterRequest {
    /// 要下线的 runtime id 列表。
    #[serde(default)]
    pub runtime_ids: Vec<String>,
    /// 可选的下线原因，按**请求里的原文 id** 索引。
    #[serde(default)]
    pub offline_reasons: std::collections::BTreeMap<String, Value>,
}

/// upstream `ClaimTasksByRuntimeRequest`（`daemon.go:1706` 附近）。
///
/// `max_tasks == 0` 是**明确不领**（服务端回 200 `{"tasks":[]}`，不折算成 1）；
/// 负数服务端回 400。`daemon_id` 必须与服务端 token 身份一致，否则 403。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimRequest {
    /// 机器标识。
    #[serde(default)]
    pub daemon_id: String,
    /// 要领取的 runtime id 列表。
    #[serde(default)]
    pub runtime_ids: Vec<String>,
    /// 上限。
    #[serde(default)]
    pub max_tasks: i64,
}

/// `POST /api/daemon/tasks/claim` 的响应体（`claims.rs::claim_batch_core`）。
///
/// 两个 hint 字段只在「没领满 + 调用方声明了 `claim-poll-hints-v1`」时出现，
/// 所以都是 `Option`；缺失时客户端退回自己的固定轮询间隔。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ClaimResponse {
    /// 领到的任务（可能为空）。
    #[serde(default)]
    pub tasks: Vec<ClaimedTask>,
    /// 服务端是否支持 `claim-poll-hints-v1`。
    #[serde(default)]
    pub claim_poll_hint_supported: bool,
    /// 下一个 deferred 任务还有多少毫秒到期（服务端已 clamp 到 ≥ 1s）。
    #[serde(default)]
    pub next_deferred_task_after_ms: Option<i64>,
}

/// claim 路径返回的任务 —— [`crate::ClaimedTask`] 的线上形状。
///
/// 服务端的 `DaemonTaskResponse` 有 30+ 字段（`taskToResponse` 的逐字段投影），
/// 客户端只取「认领之后必须用到的」这一组；`auth_token` 是 claim 路径独有的。
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct ClaimedTask {
    /// task id。
    #[serde(default)]
    pub id: String,
    /// 归属 runtime。
    #[serde(default)]
    pub runtime_id: String,
    /// 归属 issue（chat / quick-create 任务为空串）。
    #[serde(default)]
    pub issue_id: String,
    /// 归属 workspace。
    #[serde(default)]
    pub workspace_id: String,
    /// 服务端口径的状态（claim 之后恒为 `dispatched`）。
    #[serde(default)]
    pub status: String,
    /// 执行期 token（claim 路径唯一一份凭据）。
    #[serde(default)]
    pub auth_token: String,
}
