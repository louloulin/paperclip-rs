//! daemon 客户端：注册 / 心跳 / claim 三件事的**唯一**实现（M3-7 / LUM-1438）。
//!
//! ## 范围
//!
//! 本切片只做「daemon 侧的最小可用实现」：能登记自己、能维持心跳、能把待办领走。
//! 任务**执行**（execenv、adapter 拉起、worktree）是 M3-8（`LUM-1440`）的
//! `src/execenv/`；那部分只依赖这里的 [`DaemonClient::claim`] 拿到 [`ClaimedTask`]。
//!
//! ## 与上游的三条对应关系
//!
//! | 本模块 | 上游 | 说明 |
//! |--------|------|------|
//! | [`DaemonClient::register`] | `daemon.go:196` `Register` | 一次登记换回 runtime 台账 |
//! | [`DaemonClient::heartbeat`] | `daemon.go:4532` `runHeartbeatTick` | 心跳 + 待办分发 |
//! | [`DaemonClient::claim`] | `daemon.go:1706` 附近 `ClaimTasksByRuntime` | 批量认领 |
//!
//! 三处的**线形状**逐字对齐上游，唯一的结构性差别是传输：上游同一个方法在 HTTP 与
//! WS 两条腿上各有一份收尾（`SendHeartbeat` / `sendWSHeartbeat`），这里用
//! [`DaemonTransport`] 把「发字节」抽走，业务收尾只有一份。
//!
//! ## 一次心跳里 daemon 要做什么
//!
//! 上游 `handleHeartbeatActions`（`daemon.go:4553`）的本地投影是
//! [`plan_heartbeat_actions`]：ack 里的待办种类 → 要发起的动作列表。它是**纯函数**，
//! 因此「复数优先、单数兜底」这条容易写错的上游规则可以直接被单元测试钉住。
//! 真正发起这些动作（拉模型列表、导入技能、升级 CLI）属于 M3-8，这里只把清单交出去。

use serde_json::Value;

use mc_daemon_proto::messages::daemon::{
    DaemonHeartbeatAckPayload, DaemonHeartbeatPendingLocalSkillImport,
    DaemonHeartbeatPendingLocalSkills, DaemonHeartbeatPendingUpdate, DaemonHeartbeatRequestPayload,
    HEARTBEAT_STATUS_RUNTIME_GONE,
};

use crate::state::{ClientState, Registration};
use crate::transport::{DaemonTransport, TransportError};
use crate::wire::{
    ClaimRequest, ClaimResponse, ClaimedTask, DeregisterRequest, FailedProfile, RegisterRequest,
    RegisterResponse, RegisterRuntime, CLAIM_PATH, DEREGISTER_PATH, HEARTBEAT_PATH, REGISTER_PATH,
};

/// 客户端配置：登记时自报的那些字段 + 认领策略。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientConfig {
    /// 目标 workspace（字符串 UUID）。
    pub workspace_id: String,
    /// 本机 daemon id。
    pub daemon_id: String,
    /// 机器名。
    pub device_name: String,
    /// CLI 版本。
    pub cli_version: String,
    /// `"desktop"` 表示由 Electron 应用拉起；CLI 直启留空。
    pub launched_by: String,
    /// 历史 hostname 派生的 daemon id（迁移期用，正常留空）。
    pub legacy_daemon_ids: Vec<String>,
    /// 单次 claim 的上限（上游默认 1；`0` = 不领）。
    pub max_tasks: i64,
    /// 本机支不支持批量导入（心跳字段，决定服务端发单数还是复数待办）。
    pub supports_batch_import: bool,
}

impl ClientConfig {
    /// 最小配置：四个必填字段，其余取上游零值（`max_tasks = 1`）。
    #[must_use]
    pub fn new(
        workspace_id: impl Into<String>,
        daemon_id: impl Into<String>,
        device_name: impl Into<String>,
        cli_version: impl Into<String>,
    ) -> Self {
        Self {
            workspace_id: workspace_id.into(),
            daemon_id: daemon_id.into(),
            device_name: device_name.into(),
            cli_version: cli_version.into(),
            launched_by: String::new(),
            legacy_daemon_ids: Vec::new(),
            max_tasks: 1,
            supports_batch_import: true,
        }
    }
}

/// 客户端故障。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ClientError {
    /// 配置缺必填项（服务端也回 400，本地先拦一次，省一个来回）。
    #[error("invalid client config: {0}")]
    InvalidConfig(&'static str),
    /// 还没登记就想发心跳 —— 调用方的顺序错了。
    #[error("client is not registered; call register() first")]
    NotRegistered,
    /// 传输层故障。
    #[error(transparent)]
    Transport(#[from] TransportError),
}

impl ClientError {
    /// 值不值得重试（透传 [`TransportError::is_retriable`]；配置/顺序错误不重试）。
    #[must_use]
    pub fn is_retriable(&self) -> bool {
        match self {
            Self::Transport(err) => err.is_retriable(),
            Self::InvalidConfig(_) | Self::NotRegistered => false,
        }
    }

    /// 服务端说 runtime 没了（上游 `isRuntimeNotFoundError`）。
    #[must_use]
    pub fn is_not_found(&self) -> bool {
        matches!(self, Self::Transport(err) if err.is_not_found())
    }
}

/// 一次心跳的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeartbeatOutcome {
    /// 心跳成功，服务端确认了这台 runtime。
    Acked {
        /// 这一轮要发起的动作（见 [`plan_heartbeat_actions`]）。
        actions: Vec<PendingWork>,
    },
    /// 服务端说这台 runtime 不存在了（ack 的 `runtime_gone`，或 HTTP 腿的 404）。
    ///
    /// 处置是上游 `handleRuntimeGone`：把它从本机台账摘掉并重新登记，
    /// 而不是对着一个死 UUID 无限重试。
    RuntimeGone,
}

impl HeartbeatOutcome {
    /// 这一轮的动作列表（`RuntimeGone` 时为空）。
    #[must_use]
    pub fn actions(&self) -> &[PendingWork] {
        match self {
            Self::Acked { actions } => actions,
            Self::RuntimeGone => &[],
        }
    }
}

/// 心跳 ack 要求 daemon 去做的一件事（上游 `handleHeartbeatActions` 的四条分支）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PendingWork {
    /// 升级 CLI 到指定版本。
    Update {
        /// 请求 id（回报结果时用它关联）。
        request_id: String,
        /// 目标版本号。
        target_version: String,
    },
    /// 枚举本机模型列表。
    ListModels {
        /// 请求 id。
        request_id: String,
    },
    /// 枚举本机本地技能清单。
    ListLocalSkills {
        /// 请求 id。
        request_id: String,
    },
    /// 导入一条本地技能。
    ImportLocalSkill {
        /// 请求 id。
        request_id: String,
        /// 技能 key（发现路径上的稳定标识，不是名字）。
        skill_key: String,
    },
}

/// 把心跳 ack 翻成动作清单（上游 `handleHeartbeatActions`，`daemon.go:4553`）。
///
/// 上游规则，逐条照搬：
///
/// 1. 四类待办各自独立，一个 ack 可以同时带多条；
/// 2. **导入的复数键优先**：`pending_local_skill_imports` 非空时**忽略**单数键
///    （新服务端同时填两个键是为了兼容老 daemon，新 daemon 必须只处理一遍 ——
///    复数键的第一条与单数键是同一条，两个都处理就是重复导入）；
/// 3. 复数键为空时才退回单数键（老服务端形态）；
/// 4. 上游不认识的字段不产生动作 —— 这里同样只看已知字段。
#[must_use]
pub fn plan_heartbeat_actions(ack: &DaemonHeartbeatAckPayload) -> Vec<PendingWork> {
    let mut actions = Vec::new();
    if let Some(update) = &ack.pending_update {
        actions.push(update_action(update));
    }
    if let Some(models) = &ack.pending_model_list {
        actions.push(PendingWork::ListModels {
            request_id: models.id.clone(),
        });
    }
    if let Some(skills) = &ack.pending_local_skills {
        actions.push(list_skills_action(skills));
    }
    if ack.pending_local_skill_imports.is_empty() {
        if let Some(import) = &ack.pending_local_skill_import {
            actions.push(import_action(import));
        }
    } else {
        actions.extend(ack.pending_local_skill_imports.iter().map(import_action));
    }
    actions
}

fn update_action(pending: &DaemonHeartbeatPendingUpdate) -> PendingWork {
    PendingWork::Update {
        request_id: pending.id.clone(),
        target_version: pending.target_version.clone(),
    }
}

fn list_skills_action(pending: &DaemonHeartbeatPendingLocalSkills) -> PendingWork {
    PendingWork::ListLocalSkills {
        request_id: pending.id.clone(),
    }
}

fn import_action(pending: &DaemonHeartbeatPendingLocalSkillImport) -> PendingWork {
    PendingWork::ImportLocalSkill {
        request_id: pending.id.clone(),
        skill_key: pending.skill_key.clone(),
    }
}

/// 一次批量 claim 的结果。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClaimOutcome {
    /// **新**认领到的任务（已经在跑的重复项已被 [`ClientState`] 丢掉）。
    pub tasks: Vec<ClaimedTask>,
    /// 服务端支持轮询提示位（`claim-poll-hints-v1`）。
    pub claim_poll_hint_supported: bool,
    /// 距下一个 deferred 任务到期的毫秒数（服务端已 clamp 到 ≥ 1s）。
    pub next_deferred_task_after_ms: Option<i64>,
}

/// daemon 客户端：一个传输 + 一份状态。
///
/// 不做并发：调用方决定心跳节奏（上游是 per-runtime 的 ticker），这里只保证同一
/// 状态机不会被并发改坏。
pub struct DaemonClient<T: DaemonTransport> {
    transport: T,
    config: ClientConfig,
    state: ClientState,
}

impl<T: DaemonTransport> DaemonClient<T> {
    /// 新建（此时**未**登记）。
    #[must_use]
    pub fn new(transport: T, config: ClientConfig) -> Self {
        Self {
            transport,
            config,
            state: ClientState::new(),
        }
    }

    /// 当前状态（只读）。
    #[must_use]
    pub fn state(&self) -> &ClientState {
        &self.state
    }

    /// 当前配置。
    #[must_use]
    pub fn config(&self) -> &ClientConfig {
        &self.config
    }

    /// 拆掉客户端拿回传输（测试用它取回记录下来的请求）。
    #[must_use]
    pub fn into_transport(self) -> T {
        self.transport
    }

    /// `POST /api/daemon/register`。
    ///
    /// `runtimes` 是本机**自报**的可运行项；服务端认下哪些以响应为准，
    /// 所以台账的真值来自 [`Registration::runtimes`]，不是传进来的这个列表。
    ///
    /// # Errors
    ///
    /// 配置缺 `workspace_id` / `daemon_id`、或传输故障。
    pub async fn register(
        &mut self,
        runtimes: Vec<RegisterRuntime>,
        failed_profiles: Vec<FailedProfile>,
    ) -> Result<Registration, ClientError> {
        if self.config.workspace_id.is_empty() {
            return Err(ClientError::InvalidConfig("workspace_id is required"));
        }
        if self.config.daemon_id.is_empty() {
            return Err(ClientError::InvalidConfig("daemon_id is required"));
        }
        let request = RegisterRequest {
            workspace_id: self.config.workspace_id.clone(),
            daemon_id: self.config.daemon_id.clone(),
            legacy_daemon_ids: self.config.legacy_daemon_ids.clone(),
            device_name: self.config.device_name.clone(),
            cli_version: self.config.cli_version.clone(),
            launched_by: self.config.launched_by.clone(),
            runtimes,
            failed_profiles,
        };
        let body = serde_json::to_value(&request).map_err(|err| TransportError::Malformed {
            url: REGISTER_PATH.to_owned(),
            reason: err.to_string(),
        })?;
        let raw = self.transport.post_json(REGISTER_PATH, &body).await?;
        let response: RegisterResponse =
            serde_json::from_value(raw).map_err(|err| TransportError::Malformed {
                url: REGISTER_PATH.to_owned(),
                reason: err.to_string(),
            })?;

        let registration = Registration {
            workspace_id: self.config.workspace_id.clone(),
            daemon_id: self.config.daemon_id.clone(),
            // 服务端响应里没有 device_name/cli_version（它只存 runtime 台账），
            // 用本机自报值回填：这两个字段是给日志与后续 register 用的。
            device_name: self.config.device_name.clone(),
            cli_version: self.config.cli_version.clone(),
            runtimes: response.runtimes,
            repos_version: response.repos_version,
        };
        self.state.set_registration(registration.clone());
        Ok(registration)
    }

    /// `POST /api/daemon/heartbeat`（单台 runtime）。
    ///
    /// HTTP 腿的 ack 被服务端刻意剪掉了 `runtime_id` 与 `server_capabilities`
    /// （`lifecycle.rs::http_ack_value`：协议协商只走 WS），所以这里用
    /// [`DaemonHeartbeatAckPayload`] 的默认值反序列化 —— 它带 `#[serde(default)]`。
    ///
    /// # Errors
    ///
    /// 未登记（[`ClientError::NotRegistered`]）或传输故障；`404` **不是**错误，
    /// 它落 [`HeartbeatOutcome::RuntimeGone`]。
    pub async fn heartbeat(&mut self, runtime_id: &str) -> Result<HeartbeatOutcome, ClientError> {
        if self.state.registration().is_none() {
            return Err(ClientError::NotRegistered);
        }
        // 走 proto 类型序列化而不是手写 `json!`：`supports_batch_import` 在上游是
        // `omitempty`，proto 的 `skip_serializing_if` 复现了它（`false` 必须缺席，
        // 不能发成 `false` —— 老服务端按「字段在不在」区分新老 daemon）。
        let request = DaemonHeartbeatRequestPayload {
            runtime_id: runtime_id.to_owned(),
            supports_batch_import: self.config.supports_batch_import,
        };
        let body = serde_json::to_value(&request).map_err(|err| TransportError::Malformed {
            url: HEARTBEAT_PATH.to_owned(),
            reason: err.to_string(),
        })?;
        let raw = match self.transport.post_json(HEARTBEAT_PATH, &body).await {
            Ok(raw) => raw,
            Err(err) if err.is_not_found() => {
                // 服务端说这台 runtime 没了：上游把它当成 `runtime_gone` ack 处理
                // （`daemon_ws.go` 的 runtimeGoneHeartbeatAck，HTTP 腿是 404）。
                self.state.note_runtime_gone(runtime_id);
                return Ok(HeartbeatOutcome::RuntimeGone);
            }
            Err(err) => return Err(err.into()),
        };
        let ack = parse_ack(&raw)?;
        if ack.runtime_gone || ack.status == HEARTBEAT_STATUS_RUNTIME_GONE {
            self.state.note_runtime_gone(runtime_id);
            return Ok(HeartbeatOutcome::RuntimeGone);
        }
        self.state.mark_heartbeat(runtime_id, chrono::Utc::now());
        Ok(HeartbeatOutcome::Acked {
            actions: plan_heartbeat_actions(&ack),
        })
    }

    /// 给本机**所有**还活着的 runtime 各发一次心跳（上游是按 runtime 起 ticker，
    /// 这里给一个同步入口，节奏仍由调用方掌握）。
    ///
    /// 一台失败就整体返回错误 —— 已成功的那几台的水位已经被记下了，重入安全。
    ///
    /// # Errors
    ///
    /// 未登记或传输故障。
    pub async fn heartbeat_all(&mut self) -> Result<Vec<(String, HeartbeatOutcome)>, ClientError> {
        let ids = self.state.live_runtime_ids();
        let mut out = Vec::with_capacity(ids.len());
        for runtime_id in ids {
            let outcome = self.heartbeat(&runtime_id).await?;
            out.push((runtime_id, outcome));
        }
        Ok(out)
    }

    /// `POST /api/daemon/tasks/claim`（批量的两条等价路径之一）。
    ///
    /// 两个**不发包**的短路，都对着服务端已验证的语义：
    ///
    /// - 本机没有活着的 runtime：服务端对空 `runtime_ids` 本来也只回 `{"tasks":[]}`；
    /// - `max_tasks <= 0`：`0` 服务端定义成「明确不领」（回空列表），负数服务端回 400
    ///   —— 两种都不该发出去。
    ///
    /// # Errors
    ///
    /// 传输故障。重复的认领不返回 —— 它们已被 [`ClientState::accept_claims`] 丢掉。
    pub async fn claim(&mut self) -> Result<ClaimOutcome, ClientError> {
        let runtime_ids = self.state.live_runtime_ids();
        if runtime_ids.is_empty() || self.config.max_tasks <= 0 {
            return Ok(ClaimOutcome::default());
        }
        let request = ClaimRequest {
            daemon_id: self.config.daemon_id.clone(),
            runtime_ids,
            max_tasks: self.config.max_tasks,
        };
        let body = serde_json::to_value(&request).map_err(|err| TransportError::Malformed {
            url: CLAIM_PATH.to_owned(),
            reason: err.to_string(),
        })?;
        let raw = self.transport.post_json(CLAIM_PATH, &body).await?;
        let response: ClaimResponse =
            serde_json::from_value(raw).map_err(|err| TransportError::Malformed {
                url: CLAIM_PATH.to_owned(),
                reason: err.to_string(),
            })?;
        let tasks = self.state.accept_claims(response.tasks);
        Ok(ClaimOutcome {
            tasks,
            claim_poll_hint_supported: response.claim_poll_hint_supported,
            next_deferred_task_after_ms: response.next_deferred_task_after_ms,
        })
    }

    /// `POST /api/daemon/deregister`：把这些 runtime 标下线。
    ///
    /// `runtime_ids` 为空时下线本机全部活着的 runtime（进程收到关闭信号的形态）。
    /// `offline_reasons` 按**请求里的原文 id** 索引；形状不限（服务端原样透传）。
    ///
    /// # Errors
    ///
    /// 未登记或传输故障。
    pub async fn deregister(
        &mut self,
        runtime_ids: Vec<String>,
        offline_reasons: std::collections::BTreeMap<String, Value>,
    ) -> Result<(), ClientError> {
        if self.state.registration().is_none() {
            return Err(ClientError::NotRegistered);
        }
        let ids = if runtime_ids.is_empty() {
            self.state.live_runtime_ids()
        } else {
            runtime_ids
        };
        let request = DeregisterRequest {
            runtime_ids: ids.clone(),
            offline_reasons,
        };
        let body = serde_json::to_value(&request).map_err(|err| TransportError::Malformed {
            url: DEREGISTER_PATH.to_owned(),
            reason: err.to_string(),
        })?;
        self.transport.post_json(DEREGISTER_PATH, &body).await?;
        self.state.forget_runtimes(&ids);
        Ok(())
    }

    /// 一条任务跑完了：把它移出本机台账（服务端重派时才不会被客户端自己挡掉）。
    ///
    /// 返回认领它的 runtime id。
    pub fn finish_task(&mut self, task_id: &str) -> Option<String> {
        self.state.finish_task(task_id)
    }
}

/// HTTP 腿的心跳 ack 解析（缺字段用默认值 —— 服务端刻意剪掉了两个键）。
fn parse_ack(raw: &Value) -> Result<DaemonHeartbeatAckPayload, TransportError> {
    serde_json::from_value(raw.clone()).map_err(|err| TransportError::Malformed {
        url: HEARTBEAT_PATH.to_owned(),
        reason: err.to_string(),
    })
}
